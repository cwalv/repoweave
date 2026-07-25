//! Integration trait and context.
//!
//! Integrations are pluggable units that derive config for one ecosystem tool
//! from the repo list. Each integration participates in activation (write path)
//! and check (read-only inspection).

use crate::manifest::{IntegrationConfig, ProjectName, RepoEntry, RepoPath, WorkweaveConfig};
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Integration context — shared input for all integrations
// ---------------------------------------------------------------------------

/// Everything an integration needs to do its work.
///
/// Constructed once per activation/check cycle and passed to each integration.
/// Immutable — integrations read this, then write to the filesystem.
pub struct IntegrationContext<'a> {
    /// The directory an integration's managed and generated files live in:
    /// `<workspace_root>/projects/<project>`, in every weave and for every
    /// verb. Write hooks author here and read hooks inspect here, so a finding
    /// names the same path whichever verb produced it.
    ///
    /// This is **not** the weave root. The same files are surfaced there as
    /// symlinks, but only for the *active* project and only when the source
    /// exists — a view that answers a different question, and one that
    /// [`crate::workspace::WorkspaceSession::context_base`] makes impossible
    /// to bind here. Whether the root carries the symlink is
    /// [`crate::activate::verify_surfacing`]'s axis, not an integration's.
    pub output_dir: &'a Path,

    /// The weave root: the directory holding `projects/` and the registry
    /// dirs the repos live under. Used for detecting manifest files (e.g.,
    /// `Cargo.toml`, `package.json`) inside repos, which is why it is distinct
    /// from `output_dir` — the repos are siblings of `projects/`, not children
    /// of the project directory. Bound to the weave the verb is acting on:
    /// the primary root at primary, the workweave directory inside one.
    pub workspace_root: &'a Path,

    /// The active project name.
    pub project: &'a ProjectName,

    /// Repo entries from the project's `rwv.yaml`, as an ordered list of
    /// `(path, entry)` pairs. Sorted by `RepoPath` (matches the BTreeMap
    /// iteration order in the manifest). Integrations only iterate this
    /// field; no random-access lookups are needed.
    pub repos: Vec<(RepoPath, RepoEntry)>,

    /// Per-integration config from the `integrations:` key in `rwv.yaml`.
    pub config: &'a IntegrationConfig,

    /// All git repos found on disk under registry directories (relative paths).
    /// Computed once, shared across integrations.
    pub all_repos_on_disk: &'a [RepoPath],

    /// All project paths (e.g., `["web-app", "mobile-app"]`).
    /// Computed once, shared across integrations.
    pub all_project_paths: &'a [String],

    /// Pre-computed detection cache mapping manifest filenames to lists of
    /// repo paths that contain that manifest. Populated once per
    /// activation/check cycle before any integrations run.
    pub detection_cache: &'a HashMap<String, Vec<String>>,

    /// The project's `workweave:` config from `rwv.yaml`, if any.
    ///
    /// Made visible so integrations can detect cross-section collisions
    /// (e.g. a name claimed by both `static-files.files` and
    /// `workweave.link` — see rwv-c5h / plan §5h). Defaults to `None` for
    /// projects with no `workweave:` section. Integrations that don't care
    /// about workweave config should leave this untouched.
    pub workweave: Option<&'a WorkweaveConfig>,
}

impl<'a> IntegrationContext<'a> {
    /// Repos that should appear in ecosystem workspace configs.
    /// Excludes `reference` repos — they're read-only, not part of the build graph.
    pub fn active_repos(&self) -> impl Iterator<Item = (&RepoPath, &RepoEntry)> {
        self.repos
            .iter()
            .filter(|(_, e)| e.role.is_active())
            .map(|(rp, e)| (rp, e))
    }

    /// Active repos whose directory contains a given manifest file.
    ///
    /// Shared helper for ecosystem integrations (npm, pnpm, Go, uv, Cargo)
    /// that all need the same "find repos with manifest X" logic.
    ///
    /// Uses `workspace_root` (not `output_dir`) to check for manifest files,
    /// so that repo detection works even when the output directory differs
    /// from where repos live (e.g., in weaves).
    pub fn detect_repos_with_manifest(&self, filename: &str) -> Vec<String> {
        if let Some(cached) = self.detection_cache.get(filename) {
            return cached.clone();
        }
        detect_repos_with_manifest_impl(self.workspace_root, &self.repos, filename)
    }
}

/// Perform a live filesystem scan for active repos that contain `filename`.
///
/// Shared by [`IntegrationContext::detect_repos_with_manifest`] (as a fallback)
/// and the cache pre-computation in the integration runner.
pub fn detect_repos_with_manifest_impl(
    workspace_root: &Path,
    repos: &[(RepoPath, RepoEntry)],
    filename: &str,
) -> Vec<String> {
    let mut paths: Vec<String> = repos
        .iter()
        .filter(|(_, e)| e.role.is_active())
        .filter(|(rp, _)| workspace_root.join(rp.as_str()).join(filename).exists())
        .map(|(rp, _)| rp.as_str().to_string())
        .collect();
    paths.sort();
    paths
}

// ---------------------------------------------------------------------------
// Check results
// ---------------------------------------------------------------------------

/// Severity of an issue found by a check hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// A single issue reported by an integration's check or verify hook.
///
/// `safe_to_fix`: when `true` (the default for all environment/config issues),
/// `rwv doctor --fix` may invoke the integration's write path to repair. When
/// `false`, the issue is for human attention only — `doctor --fix` prints it
/// but does NOT attempt an automated write. Use `false` for USER-HELD findings
/// where the user explicitly holds the pen and auto-repair would be unsafe.
#[derive(Debug, Clone)]
pub struct Issue {
    pub integration: String,
    pub severity: Severity,
    pub message: String,
    /// Whether `rwv doctor --fix` is permitted to auto-repair this finding.
    ///
    /// `true` for all environment / config / drift issues (the common case).
    /// `false` for USER-HELD findings where the user holds the pen on a managed
    /// file region and automatic overwrite would silently destroy user content.
    pub safe_to_fix: bool,
}

// ---------------------------------------------------------------------------
// The Integration trait
// ---------------------------------------------------------------------------

/// A pluggable unit that derives config for one tool from the repo list.
///
/// Integrations are stateless — all input comes through `IntegrationContext`,
/// all output goes to the filesystem or is returned as `Issue`s.
///
/// Built-in integrations are compiled in. The trait is object-safe so that
/// future versions can load integrations dynamically (e.g., from shared
/// libraries or WASM modules) and store them as `Box<dyn Integration>`.
pub trait Integration {
    /// Unique identifier (e.g., `"npm-workspaces"`).
    fn name(&self) -> &str;

    /// Whether this integration runs without explicit opt-in.
    fn default_enabled(&self) -> bool;

    /// Generate config files and run install commands.
    /// Called during `rwv activate`, workweave creation, `rwv sync`, `rwv add`, and `rwv remove`.
    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()>;

    /// Remove generated files. Called during deactivation.
    fn deactivate(&self, root: &Path) -> anyhow::Result<()>;

    /// Read-only inspection. Returns issues without changing state.
    /// Called by `rwv doctor`.
    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>>;

    /// Activate hook — run after `rwv activate` generates config files,
    /// before activation completes.
    ///
    /// Integrations override this to run ecosystem install commands
    /// (e.g., `npm install`, `uv sync`, `cargo generate-lockfile`) that
    /// follow membership changes. Fires whenever the workspace's set of
    /// active repos may have changed; users can suppress with
    /// `rwv activate --no-install`.
    ///
    /// This hook was previously named `lock` (fired on `rwv lock`); the
    /// trigger for ecosystem-lockfile refresh is workspace membership
    /// change, which is what `rwv activate` represents.
    fn activate_hook(&self, _ctx: &IntegrationContext) -> anyhow::Result<()> {
        Ok(())
    }

    /// Return the filenames (relative to `output_dir`) that this integration
    /// **fully owns** — rwv writes 100% of the content, no user-authored
    /// surface, whole-write and whole-delete safe, gitignore-eligible.
    ///
    /// Examples: lockfiles (`Cargo.lock`, `package-lock.json`), gita CSVs,
    /// a fully-owned `.code-workspace`.
    ///
    /// Every file here is symlinked from the weave root into the project
    /// directory.
    ///
    /// The default returns an empty list.
    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<String> {
        Vec::new()
    }

    /// Return the filenames (relative to `output_dir`) that this integration
    /// **manages a region of** — hybrid files, where rwv owns a declared
    /// key/region set inside a user-authored file. Symlinked, **never
    /// gitignored**, strip-only on deactivate (delete-if-rwv-header — never
    /// blow away user content).
    ///
    /// Examples: a hybrid `Cargo.toml` (rwv owns `[workspace].members`),
    /// `pyproject.toml` (rwv owns `[tool.uv.workspace].members`),
    /// `pnpm-workspace.yaml`, `go.work`, `package.json` (with the rwv marker),
    /// `vscode-workspace.json` when it has user content.
    ///
    /// **Default impl returns `generated_files(ctx)`** — this is the safe
    /// default for existing integrations: every file currently declared in
    /// `generated_files()` continues to participate in symlink surfacing
    /// unchanged. As each integration is ported, it
    /// moves the hybrid entries from `generated_files()` to `managed_files()`
    /// explicitly. Integrations that have no hybrid files (e.g. gita) may
    /// leave the default in place — the union of the two methods is what
    /// drives surfacing, so duplication is harmless.
    ///
    /// **Invariant the two methods together imply:** the union of
    /// `generated_files(ctx)` and `managed_files(ctx)` is the complete set
    /// of root-relative paths that the framework will symlink for this
    /// integration. Files that appear in BOTH are coalesced by the union;
    /// no integration should declare the same path with conflicting
    /// ownership semantics within itself.
    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        self.generated_files(ctx)
    }

    /// Verify that the on-disk managed/generated artifacts agree with what
    /// `activate()` would produce for the current `rwv.yaml` + `rwv.lock`.
    ///
    /// Called by context verbs (`rwv activate`, `rwv fetch`, workweave-create)
    /// and by `rwv doctor`. **Must not author content** — only inspect.
    /// Emit drift findings as `Issue`s (typically `Severity::Warning`); the
    /// recovery hatch is `rwv doctor --fix`, which re-runs `activate()`.
    ///
    /// **Default returns empty** (no drift). An integration opts in to
    /// drift detection by overriding this method — typically a byte-level
    /// or structural comparison between the on-disk managed/generated
    /// artifact and what `activate()` would produce for the current
    /// `rwv.yaml` + `rwv.lock`.
    ///
    /// `verify` is intentionally separate from `check`: `check` reports
    /// environment/config preconditions (CLI absent from PATH, manifest
    /// schema problems) and is run by `rwv doctor` and the workspace
    /// session. `verify` reports drift between intent and on-disk content,
    /// and is run by context verbs and `rwv doctor`. The two streams must
    /// not be collapsed: an environment problem like "cargo not on PATH"
    /// is not drift, and surfacing it on every `rwv activate` would be
    /// noise.
    ///
    /// Per-integration ports override this when the
    /// integration starts owning hybrid content.
    ///
    /// See [`trigger-model.md`](../docs/repoweave/integration-ownership/trigger-model.md)
    /// for the full intent-vs-context-verb split.
    fn verify(&self, _ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        Ok(Vec::new())
    }

    /// Report an `Ownership::DefaultOnly` value on this integration's managed
    /// file that is **incompatible with what the members require** — not merely
    /// different from the value rwv seeded.
    ///
    /// This is deliberately not `verify()`: `verify()` answers "does the
    /// on-disk content match what `activate()` would write", and per rule 5 a
    /// `DefaultOnly` divergence is CLEAN there — permanently, by contract. This
    /// hook answers a different question, "would the ecosystem tool accept this
    /// configuration", and the two coexist on the same file. See
    /// [`crate::integrations::merge::MemberIncompatibility`] for the category
    /// and its message discipline.
    ///
    /// **Default returns `None`** — an integration whose `DefaultOnly` keys are
    /// pure preference (cargo's `[workspace].resolver`, uv's `[tool.uv].package`,
    /// npm's `name`) has no predicate to implement, and never produces this
    /// finding.
    ///
    /// # Implementing one
    ///
    /// The predicate must be **hard**: decidable from the member files and the
    /// managed file alone, with a consequence that does not depend on anything
    /// rwv cannot see. No wall-clock, no environment probes, no "which tool is
    /// the operator running".
    ///
    /// A known **soft** candidate, recorded and deliberately not implemented:
    /// npm-workspaces' `private`. `private == false` with a `workspaces` key
    /// present is a hard error under yarn classic and is tolerated by modern
    /// npm — the file predicate is decidable, but the *consequence* depends on
    /// which package manager the operator runs, which this hook cannot observe.
    /// This category only carries statements that are true, so it stays out.
    fn member_incompatibility(
        &self,
        _ctx: &IntegrationContext,
    ) -> anyhow::Result<Option<crate::integrations::merge::MemberIncompatibility>> {
        Ok(None)
    }
}

/// Whether an integration should run, considering its default and any override.
pub fn is_enabled(integration: &dyn Integration, config: &IntegrationConfig) -> bool {
    config
        .enabled()
        .unwrap_or_else(|| integration.default_enabled())
}
