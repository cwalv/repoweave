use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use crate::integrations::merge::{
    drift_issues, keypath, missing_issue, JsonDoc, ManagedDoc, RwvGeneratedMarker,
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

/// The `settings` values rwv seeds on a fresh workspace and never overwrites.
///
/// A file holding nothing but these at their seeded values carries no user
/// choice, so it does not keep the file alive through `deactivate`.
fn seeded_settings() -> [(&'static str, serde_json::Value); 2] {
    [
        (
            "git.autoRepositoryDetection",
            serde_json::Value::String("subFolders".to_string()),
        ),
        (
            "git.repositoryScanMaxDepth",
            serde_json::Value::Number(3.into()),
        ),
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

/// Is rwv's owned region present in this document?
///
/// `folders` is where rwv writes the primary entry, so its presence is what
/// makes an unmarked file user-held — the probe `activate` defers on and
/// `verify` reports USER-HELD on.
fn owned_region_present(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    obj.contains_key("folders")
}

/// Drop the rwv-owned primary entry (`"path": "."`) from the `folders` array,
/// keeping every user-added entry. Removes the key when nothing is left.
fn remove_primary_folder(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(serde_json::Value::Array(folders)) = obj.get_mut("folders") else {
        return;
    };
    folders.retain(|f| f.get("path").and_then(|v| v.as_str()) != Some("."));
    if folders.is_empty() {
        obj.remove("folders");
    }
}

/// Does anything the user authored remain in a stripped document?
///
/// A `settings` map holding only rwv's seeded values at their seeded values is
/// rwv's own leftover; a changed value is a choice and keeps the file.
fn only_seeded_settings_remain(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    if obj.is_empty() {
        return true;
    }
    let Some(serde_json::Value::Object(settings)) = obj.get("settings") else {
        return false;
    };
    obj.len() == 1
        && settings
            .iter()
            .all(|(k, v)| seeded_settings().iter().any(|(sk, sv)| sk == k && sv == v))
}

/// Strip rwv's managed region from one `.code-workspace` file.
///
/// That region is not a flat key list: rwv owns the `folders` entry whose path
/// is `"."` and the `files.exclude` keys the marker records — not the array or
/// the map holding them. Everything else, including user-added exclude keys
/// and folder entries, survives. Without the marker the user holds the pen and
/// the file is left alone; a marker predating the recorded exclude list leaves
/// the excludes in place rather than guessing which were rwv's.
fn strip_workspace_file(path: &Path) -> anyhow::Result<()> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc = JsonDoc::<RwvGeneratedMarker>::parse(&text)
        .with_context(|| format!("parsing {}", path.display()))?;

    if !doc.has_marker(&[]) {
        return Ok(());
    }

    for key in read_prev_rwv_excludes(doc.root()) {
        doc.remove_owned(&keypath(["settings", "files.exclude", &key]));
    }
    remove_primary_folder(doc.root_mut());
    doc.remove_marker(&[]);

    if only_seeded_settings_remain(doc.root()) {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    } else {
        let stripped = doc.serialize()?;
        std::fs::write(path, stripped).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
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
        // An empty or missing file starts as an empty document.
        let mut doc = if filepath.exists() {
            let content = std::fs::read_to_string(&filepath)
                .with_context(|| format!("reading {}", filepath.display()))?;
            JsonDoc::<RwvGeneratedMarker>::parse(&content).with_context(|| {
                format!(
                    "malformed JSON in {}: fix or delete the file and re-run `rwv activate`",
                    filepath.display()
                )
            })?
        } else {
            JsonDoc::empty()
        };

        // The user holds the pen on an unmarked file that already has the
        // owned region: leave it byte-for-byte alone and let verify() report
        // it. The marker is one top-level key spanning the whole managed
        // region, so ownership cannot be split per key — authoring any part of
        // an unmarked file would stamp the marker and hand rwv the rest on the
        // next run.
        if !doc.has_marker(&[]) && owned_region_present(doc.root()) {
            return Ok(());
        }

        let obj = doc.root_mut();

        // Read back the previously-recorded rwv-owned exclude keys so we can
        // remove stale entries on this run (set-subtract-old-owned-set).
        let prev_rwv_excludes = read_prev_rwv_excludes(obj);

        // Merge the `folders` array (fix #2): primary at [0], user folders
        // preserved in their original order.
        let folders_value = merge_folders(obj, ctx.project.as_str());
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
        let merged_exclude = merge_files_exclude(obj, &rwv_exclude_keys, &prev_rwv_excludes);

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
            for (key, value) in seeded_settings() {
                if !settings_map.contains_key(key) {
                    settings_map.insert(key.to_string(), value);
                }
            }
            settings_map.insert(
                "files.exclude".to_string(),
                serde_json::Value::Object(merged_exclude),
            );
        }

        let content = doc.serialize()?;
        std::fs::write(&filepath, content)?;
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("code-workspace") {
                    continue;
                }
                strip_workspace_file(&path)?;
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
    /// - **USER-HELD** (`safe_to_fix=false`): `folders` present but NO
    ///   `rwv.generated` marker — user authored the workspace file; don't
    ///   auto-clobber. `activate` defers on the same condition.
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

        let marker_present = doc.has_marker(&[]);

        // USER-HELD is `folders` present without the marker — the same pair
        // `activate` defers on, so doctor never calls a file user-held that the
        // next intent verb would take over.
        //
        // DRIFT is the `folders[0]` primary-entry compare (NOT an array of
        // members like the other five). We serialize the on-disk and expected
        // primary tuple to a single deterministic string so the shared
        // array-compare reduces to "primary matches?". `settings.files.exclude`
        // is intentionally NOT checked — its content depends on runtime
        // all_repos_on_disk / all_project_paths sets and is not reproducible
        // from the manifest alone.
        let on_disk_primary = doc
            .root()
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
            owned_region_present(doc.root()),
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
