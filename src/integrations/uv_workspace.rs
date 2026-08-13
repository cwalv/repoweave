//! uv-workspace integration.
//!
//! Generates the `[tool.uv.workspace]` region of a hybrid root `pyproject.toml`
//! declaring every Python repo in the active project as a workspace member, then
//! lets `uv sync` produce a shared `uv.lock`.
//!
//! ## Hybrid file ownership (the uv merge port)
//!
//! `pyproject.toml` is a **hybrid** file: rwv owns a declared key set, the user
//! owns everything else. Authoring goes through the shared `TomlDoc` helper in
//! [`crate::integrations::merge`], which preserves comments, key order, inline
//! tables, triple-quoted strings, and per-key formatting byte-for-semantics
//! across `toml_edit::DocumentMut` round-trips.
//!
//! **Owned keys** (the only bytes rwv writes):
//! - `[tool.uv.workspace].members` — the member path array.
//! - `[tool.uv.sources].<name>` = `{ workspace = true }` — one entry per
//!   detected member, keyed by that member's own `[project].name` (read from
//!   its `pyproject.toml`, not the directory path). A member whose name can't
//!   be read is skipped with a warning rather than keyed by directory
//!   basename (only `workspace=true` entries; user git/url/index/path
//!   sources inside `[tool.uv.sources]` are NOT touched).
//! - `[tool.uv].package` = `false` — **`DefaultOnly`**: written only when the
//!   key is absent from the file. Required so `uv sync` accepts a non-package
//!   root on fresh files. Never overwrites a user-set value (e.g. `true`).
//!   Not stripped on deactivate — it is user-adjustable, per the
//!   `Ownership::DefaultOnly` semantics in `merge.rs`.
//!
//! **Never authored:** `[project]`, `[build-system]`, `[tool.ruff]`,
//! `[tool.black]`, `[tool.rooster]`, or any other section — those are user
//! content.
//!
//! **Marker:** the per-key TOML decoration `# managed by rwv` (a comment line
//! attached as the prefix of the `members` key). Position-independent; survives
//! reordering; never injects a top-level header into a user file. See
//! [`crate::integrations::merge::TomlDoc`] for the format details.
//!
//! **Activate** = read-or-empty → merge-set the owned keys (via
//! `merge_activate`) → write. If the file is missing it is created with only
//! the managed region (plus `package = false` via `DefaultOnly`).
//!
//! **Deactivate** = capture the marker → strip `members` (via
//! `strip_deactivate`) → strip only `{workspace=true}` source entries (custom
//! adapter) → prune empty
//! `[tool.uv.workspace]`/`[tool.uv.sources]`/`[tool.uv]`/`[tool]` → delete
//! the file iff it would otherwise be empty. Both strips are gated on the
//! marker, and the marker is read *before* the first one, because
//! `strip_deactivate` removes it — see `deactivate`.
//!
//! ## Strip-by-predicate decision (the deactivate design choice)
//!
//! The shared `strip_deactivate` helper strips whole key paths. For
//! `[tool.uv.sources]` we own ONLY entries whose value contains
//! `workspace = true` — user git/url/index/path sources must survive. Two
//! options were evaluated:
//!
//! - **(a) Bespoke adapter in uv_workspace.rs**: after `strip_deactivate`
//!   removes `members`, run a separate pass that reads the document, removes
//!   only `{workspace=true}` source entries, and prunes any empty parent tables.
//!   Then, if the whole document is empty, delete the file.
//! - **(b) Extend TomlDoc with a predicate-based stripper**: a new
//!   `remove_owned_where` method accepting a closure, wired into the shared
//!   `ManagedDoc` trait.
//!
//! **Decision: (a) bespoke adapter.** Option (b) would add API surface to
//! `ManagedDoc` for a single integration's edge case. Option (a) keeps the
//! shared helper clean and moves the specialization to the one integration that
//! needs it. The bespoke adapter is ~30 lines in this file; the logic is
//! self-contained.
//!
//! ## managed/generated split (C3)
//!
//! `pyproject.toml` moves from `generated_files()` to `managed_files()` here.
//! `uv.lock` stays in `generated_files()` — it is fully-owned by rwv and
//! gitignore-eligible.

use crate::integration::{Integration, IntegrationContext, Issue, IssueKind, OwnedPath, Severity};
use crate::integrations::merge::{
    drift_issues, holds_owned_region, keypath, merge_activate, missing_issue,
    orphaned_region_issues, strip_deactivate, toml_array_strings, KeyPath, ManagedDoc, MergeResult,
    OwnedValue, Ownership, StripOutcome, TomlDoc,
};
use anyhow::Context;
use std::path::Path;

pub struct UvWorkspace;

impl UvWorkspace {
    /// The owned key paths passed to `strip_deactivate`. These cover
    /// `members` (and the whole-table pruning of `[tool.uv.workspace]`).
    ///
    /// `[tool.uv.sources]` workspace-true entries are handled separately
    /// by the bespoke adapter (`strip_workspace_sources`).
    ///
    /// `[tool.uv].package` is **not** listed here because it is a
    /// `DefaultOnly` key — it is user-adjustable and must never be stripped
    /// on deactivate (per the `Ownership::DefaultOnly` contract in `merge.rs`).
    fn deactivate_owned_keys() -> Vec<KeyPath> {
        vec![keypath(["tool", "uv", "workspace", "members"])]
    }

    /// Remove rwv's `[tool.uv.workspace]` region and its workspace-true sources
    /// from the `pyproject.toml` under `root`, leaving user-authored content
    /// untouched.
    ///
    /// Both callers reach this from the same premise — rwv has no `members` list
    /// to author — and they differ only in why: the project is going away, or its
    /// uv membership emptied. Marker-gated and idempotent, so it is safe over an
    /// absent file and over one the user holds the pen on.
    fn strip_managed_region(root: &Path) -> anyhow::Result<()> {
        let path = root.join("pyproject.toml");
        if !path.exists() {
            return Ok(());
        }

        // strip_deactivate removes `members` (and prunes empty
        // [tool.uv.workspace]) gated on the per-key marker.
        let outcome = strip_deactivate::<TomlDoc>(&path, &Self::deactivate_owned_keys())
            .with_context(|| format!("strip-deactivate {}", path.display()))?;

        // Now strip only {workspace=true} entries from [tool.uv.sources]
        // (bespoke adapter — see module doc). Only when we owned the file: an
        // unmarked, hand-authored pyproject.toml keeps its workspace-true
        // sources, and is never emptied and deleted.
        if outcome == StripOutcome::Stripped {
            Self::strip_workspace_sources(&path)
                .with_context(|| format!("strip-workspace-sources {}", path.display()))?;
        }

        Ok(())
    }

    /// Build the **primary** `(key, value)` pairs for `merge_activate`.
    ///
    /// Only the keys that carry the `# managed by rwv` marker:
    /// - `members` = sorted array of member paths (`Ownership::Author`).
    /// - `[tool.uv].package` = `false` (`Ownership::DefaultOnly` — set only
    ///   when absent; never overwrites a user-set value).
    ///
    /// Source entries (`[tool.uv.sources].*`) are NOT included here —
    /// they are written in a second pass by `set_workspace_sources` so that
    /// only `members` carries the marker decoration (not each source entry).
    /// See `activate()` and the module doc.
    fn primary_owned_pairs(members: &[String]) -> Vec<(KeyPath, Ownership, OwnedValue)> {
        vec![
            (
                keypath(["tool", "uv", "workspace", "members"]),
                Ownership::Author,
                OwnedValue::Array(members.to_vec()),
            ),
            (
                keypath(["tool", "uv", "package"]),
                Ownership::DefaultOnly,
                OwnedValue::Bool(false),
            ),
        ]
    }

    /// Set `[tool.uv.sources].<dep_name> = { workspace = true }` for each
    /// member into the file at `path`, using a direct toml_edit pass.
    ///
    /// `dep_name` is the member's own `[project].name` (read from
    /// `<workspace_root>/<member>/pyproject.toml`) — never the directory
    /// basename, which is not a package identity uv resolves. A member whose
    /// name can't be read (missing file, parse failure, no `[project].name`)
    /// is skipped with a warning naming it, via `member_package_name`.
    ///
    /// This is a **separate pass** from `merge_activate` so that source entries
    /// do NOT carry the `# managed by rwv` marker decoration. The marker lives
    /// solely on `members`; per-source markers would cause the "exactly one
    /// marker" idempotency invariant to fail.
    ///
    /// Preserves all user-authored keys in `[tool.uv.sources]` (git/url/index/
    /// path sources).  Only sets the workspace-true entries for each member.
    fn set_workspace_sources(
        path: &Path,
        workspace_root: &Path,
        members: &[String],
    ) -> anyhow::Result<()> {
        if members.is_empty() || !path.exists() {
            return Ok(());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("parsing {} for sources set", path.display()))?;

        for member in members {
            let Some(dep_name) = member_package_name(workspace_root, member) else {
                eprintln!(
                    "[warning] uv-workspace: skipping [tool.uv.sources] entry for \
                     '{member}' — could not read [project].name from its pyproject.toml"
                );
                continue;
            };

            // Walk/create [tool][uv][sources], then set <dep_name> = { workspace = true }.
            // Using nested get_mut/insert rather than the private parent_and_leaf helper.
            if !doc.as_table().contains_key("tool") {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                doc.as_table_mut().insert("tool", toml_edit::Item::Table(t));
            }
            let tool = doc
                .as_table_mut()
                .get_mut("tool")
                .and_then(|i| i.as_table_mut())
                .ok_or_else(|| anyhow::anyhow!("tool is not a table in {}", path.display()))?;

            if !tool.contains_key("uv") {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                tool.insert("uv", toml_edit::Item::Table(t));
            }
            let uv = tool
                .get_mut("uv")
                .and_then(|i| i.as_table_mut())
                .ok_or_else(|| anyhow::anyhow!("[tool.uv] is not a table in {}", path.display()))?;

            if !uv.contains_key("sources") {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                uv.insert("sources", toml_edit::Item::Table(t));
            }
            let sources = uv
                .get_mut("sources")
                .and_then(|i| i.as_table_mut())
                .ok_or_else(|| {
                    anyhow::anyhow!("[tool.uv.sources] is not a table in {}", path.display())
                })?;

            // Only set the entry if it doesn't already exist OR if it already
            // is a `{workspace = true}` entry (idempotent update). Never
            // clobber a user-authored entry (git/url/index/path source).
            let should_set = match sources.get(&dep_name) {
                None => true,
                Some(existing) => is_workspace_true_source(existing),
            };
            if should_set {
                let mut inline = toml_edit::InlineTable::new();
                inline.insert("workspace", toml_edit::Value::from(true));
                sources.insert(
                    &dep_name,
                    toml_edit::Item::Value(toml_edit::Value::InlineTable(inline)),
                );
            }
        }

        let out = doc.to_string();
        std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Strip only `{workspace=true}` entries from `[tool.uv.sources]` using a
    /// bespoke adapter (see module doc — design decision (a)).
    ///
    /// Steps:
    /// 1. Read + parse the file (if present).
    /// 2. Iterate `[tool.uv.sources]` entries; remove those whose TOML value
    ///    is an inline table containing `workspace = true`.
    /// 3. Prune empty `[tool.uv.sources]` / `[tool.uv]` / `[tool]` tables.
    /// 4. If the document is empty after pruning → delete the file.
    ///    Else → write the stripped document back.
    ///
    /// Runs **after** `strip_deactivate` has already removed `members` and
    /// pruned `[tool.uv.workspace]`. `strip_deactivate` removes the marker as
    /// part of that strip, so this function *cannot* re-check it — by the time
    /// it runs the ownership proof is gone. The caller captures the marker
    /// BEFORE the strip and only calls this when we owned the file; calling it
    /// unconditionally would strip workspace-true sources out of a
    /// hand-authored `pyproject.toml` rwv never marked.
    fn strip_workspace_sources(path: &Path) -> anyhow::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("parsing {} for sources strip", path.display()))?;

        // Collect source-entry keys to remove (workspace = true).
        let keys_to_remove: Vec<String> = {
            let mut to_remove = Vec::new();
            // Walk to [tool.uv.sources] if present (read-only borrow).
            if let Some(tool) = doc.get("tool").and_then(|i| i.as_table()) {
                if let Some(uv) = tool.get("uv").and_then(|i| i.as_table()) {
                    if let Some(sources) = uv.get("sources").and_then(|i| i.as_table()) {
                        for (k, v) in sources.iter() {
                            if is_workspace_true_source(v) {
                                to_remove.push(k.to_string());
                            }
                        }
                    }
                }
            }
            to_remove
        };

        if keys_to_remove.is_empty() {
            // Nothing to strip.
            return Ok(());
        }

        // Remove each workspace-true source entry, then prune empty parents.
        for key in &keys_to_remove {
            // Walk to [tool][uv][sources] with mutable borrow; remove leaf.
            if let Some(tool) = doc.get_mut("tool").and_then(|i| i.as_table_mut()) {
                if let Some(uv) = tool.get_mut("uv").and_then(|i| i.as_table_mut()) {
                    if let Some(sources) = uv.get_mut("sources").and_then(|i| i.as_table_mut()) {
                        sources.remove(key);
                    }
                }
            }
        }

        // Prune: remove [tool.uv.sources] if empty, then [tool.uv] if empty,
        // then [tool] if empty. Each step re-borrows immutably first to check,
        // then mutably to remove.
        prune_if_empty(&mut doc, &["tool", "uv", "sources"]);
        prune_if_empty(&mut doc, &["tool", "uv"]);
        prune_if_empty(&mut doc, &["tool"]);

        // Check if the document is now empty.
        if doc.as_table().is_empty() {
            std::fs::remove_file(path)
                .with_context(|| format!("removing empty {}", path.display()))?;
        } else {
            let out = doc.to_string();
            std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(())
    }
}

/// Returns true if the toml_edit item represents a `{workspace = true}` source.
///
/// Accepts both inline table (`{ workspace = true }`) and regular table
/// (`[tool.uv.sources.foo]\nworkspace = true`).
fn is_workspace_true_source(item: &toml_edit::Item) -> bool {
    let table = match item {
        toml_edit::Item::Value(toml_edit::Value::InlineTable(t)) => {
            return t
                .get("workspace")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        }
        toml_edit::Item::Table(t) => t,
        _ => return false,
    };
    table
        .get("workspace")
        .and_then(|v| v.as_value())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Read `member`'s own `[project].name` from
/// `<workspace_root>/<member>/pyproject.toml`.
///
/// `None` on any failure — missing file, parse error, no `[project]` table,
/// no `name` key, or a non-string value. Callers must treat `None` as skip
/// and warn, never as license to fall back to the directory basename: that
/// basename is not a package identity uv resolves.
fn member_package_name(workspace_root: &Path, member: &str) -> Option<String> {
    let manifest = workspace_root.join(member).join("pyproject.toml");
    let text = std::fs::read_to_string(manifest).ok()?;
    let doc: toml_edit::DocumentMut = text.parse().ok()?;
    doc.get("project")?
        .as_table()?
        .get("name")?
        .as_str()
        .map(String::from)
}

/// Remove a nested key at `path` if it exists and is an empty table.
///
/// Checks (immutable borrow) whether the table at `path` is empty.
/// If so, removes it (mutable borrow). The two borrows are sequential,
/// which the borrow checker accepts.
fn prune_if_empty(doc: &mut toml_edit::DocumentMut, path: &[&str]) {
    if path.is_empty() {
        return;
    }
    // Immutable check: is the table at `path` empty?
    let is_empty = {
        let mut t: &toml_edit::Table = doc.as_table();
        let mut ok = true;
        for seg in &path[..path.len() - 1] {
            match t.get(seg) {
                Some(toml_edit::Item::Table(sub)) => t = sub,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            match t.get(path[path.len() - 1]) {
                Some(toml_edit::Item::Table(sub)) => sub.is_empty(),
                _ => false,
            }
        } else {
            false
        }
    };

    if !is_empty {
        return;
    }

    // Mutable removal: walk to parent and remove the leaf.
    let mut t: &mut toml_edit::Table = doc.as_table_mut();
    for seg in &path[..path.len() - 1] {
        match t.get_mut(seg) {
            Some(toml_edit::Item::Table(sub)) => t = sub,
            _ => return,
        }
    }
    t.remove(path[path.len() - 1]);
}

impl Integration for UvWorkspace {
    fn name(&self) -> &str {
        "uv-workspace"
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn detection_manifests(&self) -> &[&str] {
        &["pyproject.toml"]
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("pyproject.toml");
        // The authored `members` list is a function of the manifest alone.
        // Returning early instead would make it a function of history too: the
        // last member's path stays behind, in a marked key rwv still owns and
        // would no longer author.
        if paths.is_empty() {
            return Self::strip_managed_region(ctx.output_dir);
        }

        // Sort members for determinism.
        let mut members = paths;
        members.sort();

        let path = ctx.output_dir.join("pyproject.toml");

        // Step 1: merge-activate the PRIMARY owned keys only (`members` and
        // `package = false` as DefaultOnly). These are the keys that carry the
        // `# managed by rwv` marker. Sources are handled in step 2.
        let primary_owned = Self::primary_owned_pairs(&members);
        let _result: MergeResult = merge_activate::<TomlDoc>(&path, &primary_owned)
            .with_context(|| format!("merge-activate {}", path.display()))?;

        // Step 2: write `[tool.uv.sources].<dep> = { workspace = true }` for
        // each member via a direct toml_edit pass. Source entries do NOT carry
        // the marker (only `members` does) — see module doc.
        Self::set_workspace_sources(&path, ctx.workspace_root, &members)
            .with_context(|| format!("set-workspace-sources {}", path.display()))?;

        // `MergeResult.deferred` lists keys the user took the pen on.
        // Surfacing these as `Severity::Warning` issues is a C3 concern.
        // For now, silently defer — consistent with the pre-port behavior of
        // leaving hand-written sections alone.

        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        Self::strip_managed_region(root)
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let paths = ctx.detect_repos_with_manifest("pyproject.toml");
        if paths.is_empty() {
            return Ok(vec![]);
        }

        let mut issues = Vec::new();
        if which::which("uv").is_err() {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: "uv is not on PATH".to_string(),
                kind: IssueKind::ToolMissing,
                safe_to_fix: true,
            });
        }
        Ok(issues)
    }

    /// Content-correct check (Axis-2) for `pyproject.toml`.
    ///
    /// States mirrored from cargo-workspace:
    ///
    /// - **MISSING** (`safe_to_fix=true`): file absent but repos detected.
    /// - **Parse-error** (Error): malformed TOML — bail, can't assess drift.
    /// - **USER-HELD** (`safe_to_fix=false`): file present, has
    ///   `[tool.uv.workspace].members`, but NO `# managed by rwv` marker.
    /// - **DRIFT** (`safe_to_fix=true`): marker present but `members` content
    ///   diverges from what the current config would generate.
    /// - **CLEAN**: marker present and content matches.
    fn verify(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let repo_paths = ctx.detect_repos_with_manifest("pyproject.toml");
        if repo_paths.is_empty() {
            return Ok(orphaned_region_issues::<TomlDoc>(
                self.name(),
                &ctx.output_dir.join("pyproject.toml"),
                &Self::deactivate_owned_keys(),
                "rwv.toml declares no uv members, so [tool.uv.workspace] no \
                 longer belongs to rwv.",
            ));
        }

        let path = ctx.output_dir.join("pyproject.toml");

        // ── MISSING ────────────────────────────────────────────────────────
        if !path.exists() {
            return Ok(vec![missing_issue(self.name(), &path)]);
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} for verify", path.display()))?;
        let toml_doc = TomlDoc::parse(&text)
            .with_context(|| format!("parsing {} for verify", path.display()))?;
        let edit_doc: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("parsing {} for verify (toml_edit)", path.display()))?;

        let owned_keys = Self::deactivate_owned_keys();
        let marker_present = toml_doc.has_marker(&owned_keys);
        let owned_key_present =
            toml_doc.key_present(&keypath(["tool", "uv", "workspace", "members"]));

        // `members` is the only Ownership::Author key — the one checked for
        // drift. Compute expected vs on-disk and dispatch via the shared helper.
        let expected: Vec<String> = repo_paths;
        // `None` (no `members` key) is distinct from present-but-empty:
        // an absent key is always DRIFT — preserves the pre-lift Option compare.
        let on_disk = toml_array_strings(&edit_doc, &["tool", "uv", "workspace", "members"]);

        Ok(drift_issues(
            self.name(),
            &path,
            marker_present,
            owned_key_present,
            on_disk.as_deref(),
            &expected,
            "Cut over manually or add the '# managed by rwv' marker",
            "on-disk [tool.uv.workspace].members differs from rwv.toml config.",
        ))
    }

    fn activate_hook(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("pyproject.toml");
        if paths.is_empty() {
            return Ok(());
        }

        // `uv sync` installs into `.venv` and refreshes `uv.lock`.
        // Runs from workspace_root for the same reason as the other ecosystem
        // hooks (activation symlinks are in place there so uv sees the
        // workspace the user sees).
        let status = std::process::Command::new("uv")
            .arg("sync")
            .current_dir(ctx.workspace_root)
            .status()
            .context("failed to run uv")?;

        if !status.success() {
            anyhow::bail!("uv sync failed (exit {})", status);
        }

        Ok(())
    }

    /// `uv.lock` is **fully-owned** — gitignore-eligible, whole-deletable.
    /// `pyproject.toml` is **hybrid** — it is in `managed_files()`, not here.
    /// `pyproject.toml`'s marked `[tool.uv.workspace]` region, and the
    /// `uv.lock` that workspace resolved. The lock rides on the manifest's
    /// marker — see [`CargoWorkspace::owned_paths_on_disk`].
    ///
    /// [`CargoWorkspace::owned_paths_on_disk`]: crate::integrations::CargoWorkspace
    fn owned_paths_on_disk(&self, ctx: &IntegrationContext) -> Vec<OwnedPath> {
        if !holds_owned_region::<TomlDoc>(
            &ctx.output_dir.join("pyproject.toml"),
            &Self::deactivate_owned_keys(),
        ) {
            return vec![];
        }
        let mut paths = vec![OwnedPath::MarkedRegion("pyproject.toml".to_string())];
        if ctx.output_dir.join("uv.lock").is_file() {
            paths.push(OwnedPath::WholeFile("uv.lock".to_string()));
        }
        paths
    }

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("pyproject.toml").is_empty() {
            return vec![];
        }
        vec!["uv.lock".to_string()]
    }

    /// `pyproject.toml` is **hybrid** (rwv owns `[tool.uv.workspace].members`
    /// and the `{workspace=true}` entries in `[tool.uv.sources]`; the user
    /// owns everything else). It MUST NOT appear in `generated_files()` — that
    /// would mark it gitignore-eligible and whole-deletable, the exact
    /// data-loss bug the merge port fixes.
    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        let mut files = self.generated_files(ctx);
        if !ctx.detect_repos_with_manifest("pyproject.toml").is_empty() {
            files.push("pyproject.toml".to_string());
        }
        files
    }
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::IssueKind;
    use crate::manifest::{IntegrationConfig, ProjectName};
    use crate::workspace::ContainerKind;
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// A context over an empty manifest: no repo is declared, so nothing is
    /// detected and the integration has no `members` list to author.
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

    /// The `members` list rwv authored while it had members must not outlive
    /// them: an emptied membership strips it, along with the workspace-true
    /// sources that only make sense beside it, and reports it until something
    /// does.
    #[test]
    fn emptied_membership_strips_the_marked_members_list() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let path = write_file(
            &tmp,
            "pyproject.toml",
            "[project]\nname = \"demo\"\n\n[tool.uv.workspace]\n# managed by rwv\n\
             members = [\"github/test/pkg\"]\n\n[tool.uv.sources]\npkg = { workspace = true }\n",
        );

        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = ctx_over_no_members(root, &project, &config, &cache);

        let issues = UvWorkspace.verify(&ctx).unwrap();
        assert_eq!(issues.len(), 1, "expected one finding, got: {issues:?}");
        assert_eq!(issues[0].kind, IssueKind::ManagedFileDrift);
        assert!(issues[0].safe_to_fix, "the strip is the fix");

        UvWorkspace.activate(&ctx).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            !after.contains("members") && !after.contains("workspace = true"),
            "the marked region and its sources must be gone, got:\n{after}"
        );
        assert!(
            after.contains("name = \"demo\""),
            "user content must survive:\n{after}"
        );
    }

    #[test]
    fn emptied_membership_leaves_an_unmarked_members_list_alone() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let hand_written = "[tool.uv.workspace]\nmembers = [\"github/test/pkg\"]\n";
        let path = write_file(&tmp, "pyproject.toml", hand_written);

        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = ctx_over_no_members(root, &project, &config, &cache);

        assert!(
            UvWorkspace.verify(&ctx).unwrap().is_empty(),
            "with no members there is nothing to cut over to, so nothing to say"
        );
        UvWorkspace.activate(&ctx).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            hand_written,
            "rwv must not strip a list it never marked"
        );
    }

    fn write_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn is_workspace_true_recognises_inline_table() {
        // { workspace = true } inline table
        let toml = "[tool.uv.sources]\nfoo = { workspace = true }\n";
        let doc: toml_edit::DocumentMut = toml.parse().unwrap();
        let sources = doc["tool"]["uv"]["sources"].as_table().unwrap();
        let item = &sources["foo"];
        assert!(is_workspace_true_source(item));
    }

    #[test]
    fn is_workspace_true_rejects_git_source() {
        let toml = "[tool.uv.sources]\nbar = { git = \"https://example.com/bar.git\" }\n";
        let doc: toml_edit::DocumentMut = toml.parse().unwrap();
        let sources = doc["tool"]["uv"]["sources"].as_table().unwrap();
        let item = &sources["bar"];
        assert!(!is_workspace_true_source(item));
    }

    /// The member's directory basename (`py-server`) differs from its real
    /// `[project].name` (`acme-server`) — the shape the dir-basename bug
    /// requires to reproduce.
    #[test]
    fn set_workspace_sources_keys_by_project_name_not_dir_basename() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            &tmp,
            "services/py-server/pyproject.toml",
            "[project]\nname = \"acme-server\"\n",
        );
        let path = write_file(
            &tmp,
            "pyproject.toml",
            "[tool.uv.workspace]\n# managed by rwv\nmembers = [\"services/py-server\"]\n",
        );

        UvWorkspace::set_workspace_sources(&path, root, &["services/py-server".to_string()])
            .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("acme-server = { workspace = true }"),
            "sources key must be the member's [project].name; got:\n{content}"
        );
        assert!(
            !content.contains("py-server ="),
            "directory basename must not be used as the sources key; got:\n{content}"
        );
    }

    /// A member whose `pyproject.toml` is missing, and one whose
    /// `pyproject.toml` has no `[project]` table, must both be skipped rather
    /// than keyed by their directory basename — the loud-fallback contract.
    #[test]
    fn set_workspace_sources_skips_member_when_name_unreadable() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            &tmp,
            "services/py-server/pyproject.toml",
            "[project]\nname = \"acme-server\"\n",
        );
        write_file(
            &tmp,
            "services/headless/pyproject.toml",
            "[tool.ruff]\nline-length = 100\n",
        );
        // `services/ghost` has no pyproject.toml on disk at all.
        let path = write_file(
            &tmp,
            "pyproject.toml",
            "[tool.uv.workspace]\n# managed by rwv\nmembers = [\"services/ghost\", \"services/headless\", \"services/py-server\"]\n",
        );

        UvWorkspace::set_workspace_sources(
            &path,
            root,
            &[
                "services/py-server".to_string(),
                "services/ghost".to_string(),
                "services/headless".to_string(),
            ],
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("acme-server = { workspace = true }"),
            "the one readable member must still get its entry; got:\n{content}"
        );
        assert!(
            !content.contains("ghost = "),
            "member with no pyproject.toml must be skipped, not keyed by dir basename; got:\n{content}"
        );
        assert!(
            !content.contains("headless = "),
            "member with no [project].name must be skipped, not keyed by dir basename; got:\n{content}"
        );
    }

    /// `strip_workspace_sources` removes source entries by value shape
    /// (`workspace = true`), regardless of key — so it already clears an old
    /// wrong-basename entry. This pins the other half: regen must key the
    /// replacement by the member's real name, not reproduce the basename it
    /// just stripped.
    #[test]
    fn strip_then_regen_converges_wrong_basename_key_to_project_name() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            &tmp,
            "services/py-server/pyproject.toml",
            "[project]\nname = \"acme-server\"\n",
        );
        let path = write_file(
            &tmp,
            "pyproject.toml",
            r#"[tool.uv.workspace]
# managed by rwv
members = ["services/py-server"]

[tool.uv.sources]
py-server = { workspace = true }
"#,
        );

        UvWorkspace::strip_workspace_sources(&path).unwrap();
        let after_strip = std::fs::read_to_string(&path).unwrap();
        assert!(
            !after_strip.contains("py-server ="),
            "old wrong-keyed entry must be gone after strip; got:\n{after_strip}"
        );

        UvWorkspace::set_workspace_sources(&path, root, &["services/py-server".to_string()])
            .unwrap();
        let after_regen = std::fs::read_to_string(&path).unwrap();
        assert!(
            after_regen.contains("acme-server = { workspace = true }"),
            "regen must key the entry by [project].name; got:\n{after_regen}"
        );
        assert!(
            !after_regen.contains("py-server ="),
            "wrong basename key must not reappear; got:\n{after_regen}"
        );
    }

    /// `set_workspace_sources` only ever sets the current `dep_name`; it does
    /// not scan `[tool.uv.sources]` for keys it no longer expects. A stale
    /// entry from an old key therefore survives a bare re-`activate()` and
    /// sits alongside the correct one — convergence needs the strip/regen
    /// cycle pinned above, not a plain re-run.
    #[test]
    fn set_workspace_sources_alone_does_not_clear_a_stale_key() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            &tmp,
            "services/py-server/pyproject.toml",
            "[project]\nname = \"acme-server\"\n",
        );
        let path = write_file(
            &tmp,
            "pyproject.toml",
            r#"[tool.uv.workspace]
# managed by rwv
members = ["services/py-server"]

[tool.uv.sources]
py-server = { workspace = true }
"#,
        );

        UvWorkspace::set_workspace_sources(&path, root, &["services/py-server".to_string()])
            .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("acme-server = { workspace = true }"),
            "correct entry must be present; got:\n{content}"
        );
        assert!(
            content.contains("py-server = { workspace = true }"),
            "documents current behavior: a bare re-activate leaves the stale \
             basename-keyed entry in place, alongside the new correct one; \
             got:\n{content}"
        );
    }

    #[test]
    fn strip_workspace_sources_removes_workspace_true_leaves_git() {
        let tmp = TempDir::new().unwrap();
        let path = write_file(
            &tmp,
            "pyproject.toml",
            r#"[project]
name = "acme"

[tool.uv.sources]
server = { workspace = true }
some-private-lib = { git = "https://example.com/foo.git" }
"#,
        );

        UvWorkspace::strip_workspace_sources(&path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("workspace = true"),
            "ws=true must be gone"
        );
        assert!(
            content.contains("some-private-lib"),
            "git source must survive"
        );
        assert!(
            content.contains("https://example.com/foo.git"),
            "git URL must survive"
        );
        assert!(content.contains("[project]"), "[project] must survive");
    }

    #[test]
    fn strip_workspace_sources_deletes_file_when_only_workspace_sources_remain() {
        let tmp = TempDir::new().unwrap();
        let path = write_file(
            &tmp,
            "pyproject.toml",
            "[tool.uv.sources]\nserver = { workspace = true }\n",
        );

        UvWorkspace::strip_workspace_sources(&path).unwrap();
        assert!(
            !path.exists(),
            "file with only workspace sources must be deleted"
        );
    }

    #[test]
    fn strip_workspace_sources_noop_when_no_workspace_true() {
        let tmp = TempDir::new().unwrap();
        let content = "[project]\nname = \"acme\"\n\n[tool.uv.sources]\nfoo = { git = \"x\" }\n";
        let path = write_file(&tmp, "pyproject.toml", content);

        UvWorkspace::strip_workspace_sources(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, content, "noop: content must be unchanged");
    }

    /// A hand-authored `pyproject.toml` rwv never marked keeps its
    /// `{workspace = true}` sources through `deactivate`, byte-for-byte.
    ///
    /// The sources pass is not marker-gated on its own (the marker is gone by
    /// the time it runs); `deactivate` captures ownership before the strip and
    /// gates on that. Without the gate this file comes back with the
    /// `server`/`web` entries removed.
    #[test]
    fn deactivate_leaves_unmarked_workspace_sources_untouched() {
        let tmp = TempDir::new().unwrap();
        let content = r#"[project]
name = "acme"
version = "0.1.0"

# I maintain this workspace by hand, thanks.
[tool.uv.workspace]
members = ["services/server", "services/web"]

[tool.uv.sources]
server = { workspace = true }
web = { workspace = true }
some-private-lib = { git = "https://example.com/foo.git" }
"#;
        write_file(&tmp, "pyproject.toml", content);

        UvWorkspace.deactivate(tmp.path()).unwrap();

        let after = std::fs::read_to_string(tmp.path().join("pyproject.toml")).unwrap();
        assert_eq!(
            after, content,
            "unmarked pyproject.toml must survive deactivate byte-for-byte"
        );
    }

    /// The deletion path specifically: an unmarked file whose ONLY
    /// content is workspace-true sources. Ungated, the sources pass strips both
    /// entries, prunes `[tool.uv.sources]`/`[tool.uv]`/`[tool]`, finds the
    /// document empty, and `remove_file`s user-authored content.
    #[test]
    fn deactivate_does_not_delete_unmarked_file_of_only_workspace_sources() {
        let tmp = TempDir::new().unwrap();
        let content = "[tool.uv.sources]\nserver = { workspace = true }\n";
        let path = write_file(&tmp, "pyproject.toml", content);

        UvWorkspace.deactivate(tmp.path()).unwrap();

        assert!(
            path.exists(),
            "unmarked pyproject.toml must NOT be deleted by deactivate"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            content,
            "unmarked content must survive byte-for-byte"
        );
    }

    /// Regression guard — the legitimate path still strips. A marked
    /// file loses `members`, the marker, and its workspace-true sources; user
    /// sources and user sections survive.
    #[test]
    fn deactivate_still_strips_when_marked() {
        let tmp = TempDir::new().unwrap();
        let path = write_file(
            &tmp,
            "pyproject.toml",
            r#"[project]
name = "acme"

[tool.uv.workspace]
# managed by rwv
members = ["services/server"]

[tool.uv.sources]
server = { workspace = true }
some-private-lib = { git = "https://example.com/foo.git" }
"#,
        );

        UvWorkspace.deactivate(tmp.path()).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            !after.contains("workspace = true"),
            "marked file: workspace-true sources must be stripped; got:\n{after}"
        );
        assert!(
            !after.contains("managed by rwv"),
            "marked file: marker must be removed; got:\n{after}"
        );
        assert!(
            !after.contains("[tool.uv.workspace]"),
            "marked file: emptied [tool.uv.workspace] must be pruned; got:\n{after}"
        );
        assert!(
            after.contains("some-private-lib") && after.contains("[project]"),
            "user content must survive; got:\n{after}"
        );
    }

    /// The marked file-deletion path still deletes. A file whose
    /// only content is rwv's own managed region goes away on deactivate; the
    /// gate must not turn that into a leftover empty file.
    #[test]
    fn deactivate_still_deletes_marked_file_with_nothing_else_in_it() {
        let tmp = TempDir::new().unwrap();
        let path = write_file(
            &tmp,
            "pyproject.toml",
            r#"[tool.uv.workspace]
# managed by rwv
members = ["services/server"]

[tool.uv.sources]
server = { workspace = true }
"#,
        );

        UvWorkspace.deactivate(tmp.path()).unwrap();

        assert!(
            !path.exists(),
            "marked file with only rwv's region must still be deleted"
        );
    }

    #[test]
    fn primary_owned_pairs_always_includes_package_false_as_default_only() {
        let pairs = UvWorkspace::primary_owned_pairs(&["github/a/server".to_string()]);
        let package_entry = pairs
            .iter()
            .find(|(k, _, _)| k == &keypath(["tool", "uv", "package"]));
        assert!(
            package_entry.is_some(),
            "primary_owned_pairs must always include package key"
        );
        let (_, ownership, value) = package_entry.unwrap();
        assert_eq!(
            *ownership,
            Ownership::DefaultOnly,
            "package key must be DefaultOnly"
        );
        assert_eq!(
            *value,
            OwnedValue::Bool(false),
            "package default must be false"
        );
    }

    #[test]
    fn primary_owned_pairs_does_not_include_sources() {
        // Sources are handled in the set_workspace_sources pass, not via
        // primary_owned_pairs — so the marker never appears on source entries.
        let pairs = UvWorkspace::primary_owned_pairs(&["github/acme/server".to_string()]);
        let has_source = pairs
            .iter()
            .any(|(k, _, _)| k.len() >= 4 && k[2] == "sources");
        assert!(
            !has_source,
            "primary_owned_pairs must NOT include source keys (those go through set_workspace_sources)"
        );
    }
}
