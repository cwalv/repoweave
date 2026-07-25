//! go.work integration — hybrid merge port.
//!
//! # Strategy
//!
//! **PRIMARY** (when `go` is on PATH and `FORCE_GOWORK_FALLBACK` thread-local
//! is not set in tests): `go work use ./<dir>` and `go work edit
//! -dropuse=./<old>` always run; `go work edit -go=<v>` runs only when the
//! pre-edit file has no go-line yet (DefaultOnly, mirroring FALLBACK below).
//! The `go` tool round-trips `replace`/`toolchain`/`godebug` and all comments
//! via x/mod/modfile.
//!
//! **FALLBACK** (no `go` on PATH, or forced in tests): use
//! [`GoWorkDoc::merge_activate`] / [`strip_deactivate`].  Edits the `use (…)`
//! region (Author) and the leading `go <version>` line (DefaultOnly — sets the
//! default from config or `max_go_version` but never overwrites an existing
//! go-line).  All other directives survive byte-for-byte.
//!
//! The fallback is mandatory because:
//! 1. `go` is not on PATH in CI / typical test environments.
//! 2. Tests exercise it deterministically via the thread-local override.
//!
//! # max_go_version
//!
//! Used on both paths as the DefaultOnly go-version default when config does
//! not supply one. The `["go"]` entry is `Ownership::DefaultOnly` on PRIMARY
//! and FALLBACK alike: if a go-line is already present in the file it is
//! preserved unconditionally — `max_go_version` only takes effect on a fresh
//! (greenfield) or go-line-absent file.
//!
//! # Deactivate
//!
//! Uses [`strip_deactivate`] with `owned_keys = [["use"]]` only — never
//! includes `["go"]` per the C2 author's note.  Delete-if-empty is delegated
//! to [`GoWorkDoc::is_empty`] which returns true only when no `use` entries
//! AND no `replace`/`godebug`/non-comment lines beyond `go`/`toolchain`/
//! whitespace remain.
//!
//! # Test-only fallback override
//!
//! In `#[cfg(test)]` builds a thread-local `FORCE_GOWORK_FALLBACK` is
//! declared.  Tests set it to `true` to guarantee the hand-parse path is
//! taken regardless of whether `go` happens to be on PATH in the test runner.

use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use crate::integrations::merge::{
    drift_issues, keypath, merge_activate, missing_issue, strip_deactivate, GoWorkDoc, ManagedDoc,
    OwnedValue, Ownership,
};
use crate::manifest::GoWorkConfig;
use anyhow::Context;
use std::path::Path;

pub struct GoWork;

// ---------------------------------------------------------------------------
// Test-only PATH override
// ---------------------------------------------------------------------------

#[cfg(test)]
std::thread_local! {
    /// Set to `true` inside a test to force the hand-parse fallback even when
    /// `go` is on PATH.  Reset to `false` after each test (each test is a
    /// separate thread invocation).
    static FORCE_GOWORK_FALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn go_on_path() -> bool {
    #[cfg(test)]
    {
        let forced = FORCE_GOWORK_FALLBACK.with(|f| f.get());
        if forced {
            return false;
        }
    }
    which::which("go").is_ok()
}

// ---------------------------------------------------------------------------
// Integration impl
// ---------------------------------------------------------------------------

impl Integration for GoWork {
    fn name(&self) -> &str {
        "go-work"
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("go.mod");
        if paths.is_empty() {
            return Ok(());
        }

        let go_work_path = ctx.output_dir.join("go.work");

        // ── USER-HELD OWNERSHIP GUARD ──────────────────────────────────────
        // Parity with cargo-workspace: if go.work already exists with a `use`
        // block but NO `// managed by repoweave` marker, the user holds the
        // pen. Leave the file byte-for-byte unchanged and return without
        // dispatching to either the go-tool or hand-edit path.
        //
        // This guard must live here (before the path split) so it covers both:
        //   - `activate_via_go_tool`: would unconditionally run `go work use`
        //     and inject the marker via `ensure_marker_present`.
        //   - `activate_via_hand_edit`: `merge_activate` defers the `use` key
        //     but still serializes + writes the file (could mutate bytes when
        //     a go-version default is injected).
        if go_work_path.exists() {
            let text = std::fs::read_to_string(&go_work_path).with_context(|| {
                format!("reading {} for ownership check", go_work_path.display())
            })?;
            let doc = GoWorkDoc::parse(&text).with_context(|| {
                format!("parsing {} for ownership check", go_work_path.display())
            })?;
            let owned_keys = vec![keypath(["use"])];
            if !doc.has_marker(&owned_keys) && doc.key_present(&keypath(["use"])) {
                // User-held: use block present but no rwv marker.
                // Do NOT clobber the file. The ownership condition is surfaced
                // structurally by verify() via drift_issues() (Severity::Warning,
                // safe_to_fix=false), consistent with all other hybrid integrations.
                // No ad-hoc eprintln here — callers see the Issue through the
                // standard Issue stream.
                return Ok(());
            }
        }

        // Parse per-integration config (tolerates absent block).
        let cfg: GoWorkConfig = ctx.config.settings().unwrap_or_default();

        // Determine the go-version default, if any: explicit config wins,
        // else the max across member go.mod files. Both paths apply this as
        // Ownership::DefaultOnly — only when the target file's go-line is
        // currently absent; an existing go-line is never overwritten.
        let go_version_override: Option<String> = cfg
            .go_version
            .clone()
            .or_else(|| max_go_version(&paths, ctx.workspace_root));

        if go_on_path() {
            activate_via_go_tool(
                &go_work_path,
                &paths,
                go_version_override.as_deref(),
                ctx.workspace_root,
            )?;
        } else {
            activate_via_hand_edit(&go_work_path, &paths, go_version_override.as_deref())?;
        }

        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join("go.work");
        // owned_keys = [["use"]] only — NEVER include ["go"] per C2 note.
        let owned_keys = vec![keypath(["use"])];
        strip_deactivate::<GoWorkDoc>(&path, &owned_keys)
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let paths = ctx.detect_repos_with_manifest("go.mod");
        if paths.is_empty() {
            return Ok(vec![]);
        }

        let mut issues = Vec::new();
        if which::which("go").is_err() {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: "go is not on PATH; using hand-edit fallback for go.work".to_string(),
                safe_to_fix: true,
            });
        }
        Ok(issues)
    }

    /// Content-correct check (Axis-2) for `go.work`.
    ///
    /// States mirrored from cargo-workspace:
    ///
    /// - **MISSING** (`safe_to_fix=true`): file absent but repos detected.
    /// - **USER-HELD** (`safe_to_fix=false`): file present, has a `use (...)` block,
    ///   but NO `// managed by repoweave` marker.
    /// - **DRIFT** (`safe_to_fix=true`): marker present but `use` entries diverge
    ///   from what the current config would generate.
    /// - **CLEAN**: marker present and content matches.
    ///
    /// Note: always uses the fallback path for comparison (GoWorkDoc) so verify()
    /// is deterministic regardless of whether `go` is on PATH.
    fn verify(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        use crate::integrations::merge::ManagedDoc;

        let repo_paths = ctx.detect_repos_with_manifest("go.mod");
        if repo_paths.is_empty() {
            return Ok(vec![]);
        }

        let path = ctx.output_dir.join("go.work");

        // ── MISSING ────────────────────────────────────────────────────────
        if !path.exists() {
            return Ok(vec![missing_issue(self.name(), &path)]);
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} for verify", path.display()))?;
        let doc = GoWorkDoc::parse(&text)
            .with_context(|| format!("parsing {} for verify", path.display()))?;

        let owned_keys = vec![keypath(["use"])];
        let marker_present = doc.has_marker(&owned_keys);
        let owned_key_present = doc.key_present(&keypath(["use"]));

        // Only `use` (Ownership::Author) is checked for drift here. `go` is
        // Ownership::DefaultOnly: once present, any on-disk value is CLEAN by
        // contract — activate() never overwrites an existing go-line, so
        // flagging its value as drift would report a finding `--fix` cannot
        // actually resolve. Same exclusion cargo-workspace applies to its
        // DefaultOnly `resolver` key.
        //
        // Compare on-disk `use` entries against what activate() would write
        // (`./<repo-path>` entries). The shared helper sorts + dedups both
        // sides before comparing and dispatches USER-HELD → DRIFT → CLEAN.
        let expected: Vec<String> = repo_paths.iter().map(|p| format!("./{}", p)).collect();
        // `read_current_uses_from_file` returns an empty vec when the use block
        // is absent (never `None`); pre-lift compared that empty vec directly,
        // so pass `Some`.
        let on_disk = read_current_uses_from_file(&path);

        Ok(drift_issues(
            self.name(),
            &path,
            marker_present,
            owned_key_present,
            Some(&on_disk),
            &expected,
            "Cut over manually or add the '// managed by repoweave' marker",
            "on-disk use entries differ from rwv.yaml config.",
        ))
    }

    /// go.work is HYBRID — it lives in managed_files(), not generated_files().
    /// Per plan §7.12 / C3: `generated_files()` is for fully-rwv-owned files
    /// (whole-deletable, gitignore-ok); `managed_files()` is for hybrid files
    /// that are symlinked but never gitignored or whole-deleted.
    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<String> {
        // go.sum is still fully-generated (tool-managed), so it stays here.
        // go.work itself moves to managed_files().
        vec!["go.sum".to_string()]
    }

    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("go.mod").is_empty() {
            return vec![];
        }
        vec!["go.work".to_string()]
    }
}

// ---------------------------------------------------------------------------
// PRIMARY: activate via `go work edit`
// ---------------------------------------------------------------------------

fn activate_via_go_tool(
    go_work_path: &Path,
    new_paths: &[impl AsRef<str>],
    go_version: Option<&str>,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    use std::process::Command;

    // Strategy: run all `go work` commands from workspace_root without setting
    // GOWORK.  This makes `go` operate on a go.work at workspace_root (which
    // it finds by walking up, or creates fresh).  Paths stored in go.work are
    // then `./github/chatly/protocol` — relative to workspace_root.
    //
    // After the tool operations, we copy the workspace_root/go.work into
    // go_work_path (output_dir/go.work, the committed location) and remove
    // the workspace_root copy so the only canonical file is in output_dir.
    //
    // The symlink created by the framework (root/go.work →
    // projects/<project>/go.work) points to the output_dir copy.  When `go`
    // walks up from a repo dir and finds the symlink, it resolves paths from
    // the symlink's directory (workspace_root/), so `./github/...` paths work
    // correctly.
    let work_tmp = workspace_root.join("go.work");

    // Seed work_tmp from the existing output_dir copy (preserves user content).
    if !work_tmp.exists() && go_work_path.exists() {
        std::fs::copy(go_work_path, &work_tmp)?;
    }
    // If neither exists, `go work init` (below) will create work_tmp.

    // Ownership::DefaultOnly for the go-line: `go_version` is written only
    // when absent here, checked before `go work init` seeds its own
    // (toolchain-version) go-line into a fresh file.
    let go_line_absent = !std::fs::read_to_string(&work_tmp)
        .ok()
        .and_then(|text| GoWorkDoc::parse(&text).ok())
        .is_some_and(|doc| doc.key_present(&keypath(["go"])));

    // Initialize go.work at workspace_root if needed.
    if !work_tmp.exists() {
        let status = Command::new("go")
            .args(["work", "init"])
            .current_dir(workspace_root)
            .status()?;
        if !status.success() {
            anyhow::bail!("go work init failed");
        }
    }

    if let (Some(ver), true) = (go_version, go_line_absent) {
        let status = Command::new("go")
            .args(["work", "edit", &format!("-go={ver}")])
            .current_dir(workspace_root)
            .status()?;
        if !status.success() {
            anyhow::bail!("go work edit -go={ver} failed");
        }
    }

    // Read the current `use` entries so we can dropuse stale ones.
    let current_uses = read_current_uses_from_file(&work_tmp);

    // Add all new paths (relative to workspace_root).
    for p in new_paths {
        let use_path = format!("./{}", p.as_ref());
        let status = Command::new("go")
            .args(["work", "use", &use_path])
            .current_dir(workspace_root)
            .status()?;
        if !status.success() {
            // Clean up on failure.
            let _ = std::fs::remove_file(&work_tmp);
            anyhow::bail!("go work use {use_path} failed");
        }
    }

    // Drop entries no longer in new_paths.
    let new_set: std::collections::BTreeSet<String> = new_paths
        .iter()
        .map(|p| format!("./{}", p.as_ref()))
        .collect();
    for old in current_uses {
        if !new_set.contains(&old) {
            let status = Command::new("go")
                .args(["work", "edit", &format!("-dropuse={old}")])
                .current_dir(workspace_root)
                .status()?;
            if !status.success() {
                // Non-fatal: entry may already be gone.
                eprintln!("warning: go work edit -dropuse={old} failed (non-fatal)");
            }
        }
    }

    // Inject the ownership marker above the use block in work_tmp.
    ensure_marker_present(&work_tmp)?;

    // Copy work_tmp → go_work_path only if they refer to different files on
    // disk. A path-string inequality is not sufficient: in the production
    // weave layout work_tmp (workspace_root/go.work) is a symlink to
    // go_work_path (projects/<project>/go.work), so the path strings differ
    // but both resolve to the same inode. fs::copy on a symlink-to-self
    // truncates the file. Canonicalize both sides and skip the
    // copy when they resolve to the same path.
    let same_file = match (
        std::fs::canonicalize(&work_tmp),
        std::fs::canonicalize(go_work_path),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same_file {
        if let Some(parent) = go_work_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&work_tmp, go_work_path)?;
        // Remove the workspace_root copy (it will be created as a symlink by
        // the framework once activate() returns).
        let _ = std::fs::remove_file(&work_tmp);
    }
    // When they resolve to the same file (production layout with a symlink,
    // or unit tests where output_dir == workspace_root), the file is already
    // in the right place — no copy or remove needed.

    Ok(())
}

/// Read the current `use` paths from go.work using GoWorkDoc.
fn read_current_uses_from_file(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    // Extract use entries from the block via a lightweight parse.
    let mut uses = Vec::new();
    let mut in_use_block = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("use (") || trimmed == "use(" {
            in_use_block = true;
            continue;
        }
        if in_use_block {
            if trimmed == ")" {
                break;
            }
            if !trimmed.is_empty() && !trimmed.starts_with("//") {
                uses.push(trimmed.to_string());
            }
        } else if trimmed.starts_with("use ") && !trimmed.contains('(') {
            // Single-line form.
            let path_part = trimmed.strip_prefix("use ").unwrap_or("").trim();
            uses.push(path_part.to_string());
        }
    }
    uses
}

/// Ensure the `// managed by repoweave` marker line is present immediately
/// above the `use (…)` block in go.work (post-tool injection).
fn ensure_marker_present(path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    // Check if marker is already there via GoWorkDoc.
    let doc = GoWorkDoc::parse(&text)?;
    use crate::integrations::merge::ManagedDoc;
    if doc.has_marker(&[keypath(["use"])]) {
        return Ok(());
    }
    // Inject: find the `use (` line and insert the marker above it.
    let mut lines: Vec<&str> = text.lines().collect();
    let mut insert_at: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("use (")
            || trimmed == "use("
            || (trimmed.starts_with("use ") && !trimmed.contains('('))
        {
            insert_at = Some(i);
            break;
        }
    }
    if let Some(idx) = insert_at {
        lines.insert(idx, "// managed by repoweave");
        let trailing = text.ends_with('\n');
        let mut out = lines.join("\n");
        if trailing || !out.ends_with('\n') {
            out.push('\n');
        }
        std::fs::write(path, out)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// FALLBACK: activate via GoWorkDoc hand-edit
// ---------------------------------------------------------------------------

fn activate_via_hand_edit(
    go_work_path: &Path,
    new_paths: &[impl AsRef<str>],
    go_version_default: Option<&str>,
) -> anyhow::Result<()> {
    // Build owned keys.
    // ["use"] is always Author-owned.
    // ["go"] is DefaultOnly — rwv provides a default (from config or
    // max_go_version) but never overwrites an existing go-line. This
    // preserves "go 1.26" in an existing file even when config.go_version
    // is None (fixing the C11 downgrade bug without a is_some() guard).
    let use_items: Vec<String> = new_paths
        .iter()
        .map(|p| format!("./{}", p.as_ref()))
        .collect();

    let mut owned: Vec<(Vec<String>, Ownership, OwnedValue)> = vec![(
        keypath(["use"]),
        Ownership::Author,
        OwnedValue::sorted_array(use_items),
    )];

    if let Some(ver) = go_version_default {
        owned.push((
            keypath(["go"]),
            Ownership::DefaultOnly,
            OwnedValue::String(ver.to_string()),
        ));
    }

    merge_activate::<GoWorkDoc>(go_work_path, &owned)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// max_go_version — read go <version> from each go.mod, return the maximum.
// Used only when config.go_version is None and we need a version for the
// `go work edit -go=<v>` primary path call.
// ---------------------------------------------------------------------------

fn max_go_version(paths: &[impl AsRef<str>], workspace_root: &Path) -> Option<String> {
    let mut max: Option<(u64, u64)> = None;
    for p in paths {
        let go_mod = workspace_root.join(p.as_ref()).join("go.mod");
        if let Ok(content) = std::fs::read_to_string(go_mod) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("go ") {
                    let parts: Vec<&str> = rest.trim().splitn(3, '.').collect();
                    if parts.len() >= 2 {
                        if let (Ok(maj), Ok(min)) =
                            (parts[0].parse::<u64>(), parts[1].parse::<u64>())
                        {
                            if max.is_none_or(|m| (maj, min) > m) {
                                max = Some((maj, min));
                            }
                        }
                    }
                    break;
                }
            }
        }
    }
    max.map(|(maj, min)| format!("{maj}.{min}"))
}

// ---------------------------------------------------------------------------
// Tests — plan §6 go-work scenarios 1-4
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Manifest, ProjectName, Role};
    use std::collections::HashMap;
    use tempfile::TempDir;

    // Force the hand-parse fallback for all tests in this module.
    // This is idempotent and deterministic: each test body calls this before
    // exercising the integration.  go_on_path() reads the thread-local.
    fn force_fallback() {
        FORCE_GOWORK_FALLBACK.with(|f| f.set(true));
    }

    fn make_manifest_local(repos: Vec<(&str, Role)>) -> Manifest {
        let mut yaml = String::from("repositories:\n");
        for (path, role) in &repos {
            let last = path.split('/').next_back().unwrap();
            yaml.push_str(&format!(
                "  {path}:\n    type: git\n    url: https://github.com/test/{last}.git\n    version: main\n    role: {}\n",
                role.as_str()
            ));
        }
        Manifest::from_yaml_str(&yaml).unwrap()
    }

    fn make_ctx_local<'a>(
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

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    use crate::manifest::IntegrationConfig;

    // -----------------------------------------------------------------------
    // Scenario 1: Adding a repo preserves a hand-authored `replace` + comment;
    //             `go 1.26` UNCHANGED (config None).
    // -----------------------------------------------------------------------

    #[test]
    fn scenario1_adding_repo_preserves_replace_and_go_version() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Pre-existing go.work with replace directive (no marker — no use block yet).
        let seed = "go 1.26\n\n// pin local fork of legacy\nreplace example.com/legacy => ./vendor/legacy\n";
        write_file(root, "go.work", seed);

        // Two repos with go.mod files.
        touch(root, "github/test/repoweave/go.mod");
        touch(root, "github/test/some-go-tool/go.mod");

        let manifest = make_manifest_local(vec![
            ("github/test/repoweave", Role::Owned),
            ("github/test/some-go-tool", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // use block has both repos.
        assert!(
            text.contains("./github/test/repoweave"),
            "use entry missing: {text}"
        );
        assert!(
            text.contains("./github/test/some-go-tool"),
            "use entry missing: {text}"
        );

        // go 1.26 UNCHANGED (config None → no go-line write in fallback).
        assert!(
            text.contains("go 1.26"),
            "go 1.26 must be preserved: {text}"
        );
        assert!(
            !text.contains("go 1.21"),
            "must not downgrade to 1.21: {text}"
        );

        // replace block and comment survive.
        assert!(
            text.contains("replace example.com/legacy"),
            "replace must survive: {text}"
        );
        assert!(
            text.contains("// pin local fork"),
            "comment must survive: {text}"
        );

        // Ownership marker is present.
        assert!(
            text.contains("// managed by repoweave"),
            "marker must be present: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 2: Removing a repo strips its use entry; toolchain + godebug +
    //             `go 1.26` survive.
    // -----------------------------------------------------------------------

    #[test]
    fn scenario2_removing_repo_keeps_toolchain_and_godebug() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Pre-existing go.work with three entries under the marker.
        let seed = concat!(
            "go 1.26\n\n",
            "toolchain go1.26.0\n\n",
            "godebug default=go1.26\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            "\t./github/test/some-go-tool\n",
            "\t./github/test/another-module\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        // Only two repos remain in the manifest (another-module removed).
        touch(root, "github/test/repoweave/go.mod");
        touch(root, "github/test/some-go-tool/go.mod");

        let manifest = make_manifest_local(vec![
            ("github/test/repoweave", Role::Owned),
            ("github/test/some-go-tool", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // Removed entry is gone.
        assert!(
            !text.contains("another-module"),
            "removed entry must be gone: {text}"
        );

        // Remaining entries present.
        assert!(
            text.contains("./github/test/repoweave"),
            "repoweave must remain: {text}"
        );
        assert!(
            text.contains("./github/test/some-go-tool"),
            "some-go-tool must remain: {text}"
        );

        // toolchain, godebug, go 1.26 survive.
        assert!(
            text.contains("toolchain go1.26.0"),
            "toolchain must survive: {text}"
        );
        assert!(
            text.contains("godebug default=go1.26"),
            "godebug must survive: {text}"
        );
        assert!(text.contains("go 1.26"), "go 1.26 must survive: {text}");
    }

    // -----------------------------------------------------------------------
    // Scenario 3: Deactivate strips the use set but keeps replace + go 1.26.
    // -----------------------------------------------------------------------

    #[test]
    fn scenario3_deactivate_strips_use_keeps_replace() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let seed = concat!(
            "go 1.26\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            "\t./github/test/some-go-tool\n",
            ")\n\n",
            "replace example.com/foo => ../foo\n"
        );
        write_file(root, "go.work", seed);

        let integration = GoWork;
        integration.deactivate(root).unwrap();

        // File still exists — replace + go line are user content.
        assert!(
            root.join("go.work").exists(),
            "file must survive (user content present)"
        );

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // use block gone.
        assert!(
            !text.contains("./github/test/repoweave"),
            "use entry must be stripped: {text}"
        );
        assert!(
            !text.contains("use ("),
            "use block must be stripped: {text}"
        );

        // Marker gone.
        assert!(
            !text.contains("// managed by repoweave"),
            "marker must be stripped: {text}"
        );

        // go 1.26 and replace survive.
        assert!(text.contains("go 1.26"), "go 1.26 must survive: {text}");
        assert!(
            text.contains("replace example.com/foo"),
            "replace must survive: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 4: Deactivate deletes when only rwv content remained.
    // -----------------------------------------------------------------------

    #[test]
    fn scenario4_deactivate_deletes_when_only_rwv_content() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // go 1.26 + use block only (no replace/godebug/toolchain).
        // After stripping use, the remaining content is just `go 1.26` which
        // GoWorkDoc::is_empty() considers "empty enough" (only go/toolchain/
        // blank/comment lines remain).
        let seed = concat!(
            "go 1.26\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        let integration = GoWork;
        integration.deactivate(root).unwrap();

        assert!(
            !root.join("go.work").exists(),
            "file must be deleted (delete-if-empty)"
        );
    }

    // -----------------------------------------------------------------------
    // Guard: deactivate with no marker is a no-op (user holds the pen).
    // -----------------------------------------------------------------------

    #[test]
    fn deactivate_no_marker_is_noop() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Hand-written go.work with no marker.
        let seed = "go 1.26\n\nuse (\n\t./mine\n)\n";
        write_file(root, "go.work", seed);

        let integration = GoWork;
        integration.deactivate(root).unwrap();

        // File untouched.
        assert!(
            root.join("go.work").exists(),
            "hand-owned file must survive"
        );
        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            text.contains("./mine"),
            "user use entry must survive: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // USER-HELD: activate() must not clobber a user-held go.work;
    //            verify() must surface it as a structured Issue (safe_to_fix=false).
    // -----------------------------------------------------------------------

    #[test]
    fn user_held_activate_noop_and_verify_returns_issue() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Hand-written go.work: use block present but NO managed-by marker.
        let seed = "go 1.26\n\nuse (\n\t./mine\n)\n";
        write_file(root, "go.work", seed);

        // A repo with a go.mod so activate() and verify() detect repos.
        touch(root, "github/test/repoweave/go.mod");

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;

        // activate() must return Ok(()) without touching the file.
        integration.activate(&ctx).unwrap();
        let text_after = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert_eq!(
            text_after, seed,
            "activate() must not mutate a user-held go.work"
        );

        // verify() must return a single USER-HELD Issue with safe_to_fix=false.
        let issues = integration.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            !issue.safe_to_fix,
            "USER-HELD issue must have safe_to_fix=false: {issue:?}"
        );
        assert!(
            issue.message.contains("unmarked") || issue.message.contains("user content"),
            "issue message should describe the USER-HELD condition: {}",
            issue.message
        );
    }

    // -----------------------------------------------------------------------
    // Guard: go_version in config writes the go line in fallback.
    // -----------------------------------------------------------------------

    #[test]
    fn go_version_config_writes_go_line_in_fallback() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No existing go.work.
        touch(root, "github/test/repoweave/go.mod");

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project");
        // Set go_version explicitly in config.
        let config = IntegrationConfig::from_yaml("go-version: \"1.23\"");
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            text.contains("go 1.23"),
            "config go-version must be written: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Regression — no-downgrade (C11 scenario):
    //   existing go.work has `go 1.26`, config.go_version is None,
    //   but max_go_version computes 1.24 from member go.mod files.
    //   After activate, `go 1.26` must be preserved (DefaultOnly never
    //   overwrites an existing value).
    // -----------------------------------------------------------------------

    #[test]
    fn regression_no_downgrade_defaultonly_preserves_existing_go_line() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Existing go.work with go 1.26.
        let seed = concat!(
            "go 1.26\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        // Member go.mod with go 1.24 — lower than the existing 1.26.
        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n\ngo 1.24\n",
        );

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // go 1.26 must be preserved — DefaultOnly never overwrites.
        assert!(
            text.contains("go 1.26"),
            "go 1.26 must be preserved (DefaultOnly): {text}"
        );
        assert!(
            !text.contains("go 1.24"),
            "must not downgrade to 1.24 (from max_go_version): {text}"
        );
        assert!(
            !text.contains("go 1.21"),
            "must not downgrade to 1.21: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Greenfield: no existing go.work, max_go_version detects 1.22 from
    //   member go.mod. The go-line should be written at greenfield.
    // -----------------------------------------------------------------------

    #[test]
    fn greenfield_go_line_written_from_max_go_version() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No existing go.work. Member go.mod reports go 1.22.
        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n\ngo 1.22\n",
        );

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // go 1.22 written from max_go_version (DefaultOnly on a missing key).
        assert!(
            text.contains("go 1.22"),
            "go 1.22 must be written at greenfield from max_go_version: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Existing file with marker but no go-line: DefaultOnly should write the
    //   default version into the missing slot.
    // -----------------------------------------------------------------------

    #[test]
    fn existing_without_go_line_defaultonly_writes_default() {
        force_fallback();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // go.work with marker + use block but NO go-line.
        let seed = concat!(
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        // Member go.mod reports go 1.23.
        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n\ngo 1.23\n",
        );

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default(); // go_version = None
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let text = std::fs::read_to_string(root.join("go.work")).unwrap();

        // go 1.23 must be written — DefaultOnly fills an absent key.
        assert!(
            text.contains("go 1.23"),
            "go 1.23 must be written into missing go-line slot (DefaultOnly): {text}"
        );
    }

    // -----------------------------------------------------------------------
    // Guard: go-line drift (DefaultOnly) is CLEAN, mirroring cargo's
    //   resolver_default_only_drift_is_clean. The on-disk go-line is far
    //   below what the member go.mod requires; verify() must not report
    //   that as drift.
    // -----------------------------------------------------------------------

    #[test]
    fn go_directive_default_only_drift_is_clean() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let seed = concat!(
            "go 1.21\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/test/repoweave\n",
            ")\n"
        );
        write_file(root, "go.work", seed);

        write_file(
            root,
            "github/test/repoweave/go.mod",
            "module example.com/repoweave\n\ngo 1.26\n",
        );

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let issues = GoWork.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "go-line drift (DefaultOnly) must be CLEAN — got: {issues:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Guard: managed_files() returns go.work; generated_files() returns go.sum.
    // -----------------------------------------------------------------------

    #[test]
    fn managed_files_split() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/test/repoweave/go.mod");

        let manifest = make_manifest_local(vec![("github/test/repoweave", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx_local(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        let gen = integration.generated_files(&ctx);
        let man = integration.managed_files(&ctx);

        assert!(
            !gen.contains(&"go.work".to_string()),
            "go.work must not be in generated_files"
        );
        assert!(
            gen.contains(&"go.sum".to_string()),
            "go.sum must be in generated_files"
        );
        assert!(
            man.contains(&"go.work".to_string()),
            "go.work must be in managed_files"
        );
    }
}
