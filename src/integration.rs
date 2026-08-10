//! Integration trait and context.
//!
//! Integrations are pluggable units that derive config for one ecosystem tool
//! from the repo list. Each integration participates in activation (write path)
//! and check (read-only inspection).

use crate::manifest::{IntegrationConfig, ProjectName, RepoEntry, RepoPath, WorkweaveConfig};
use crate::workspace::ContainerKind;
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

    /// Which kind of container `workspace_root` is. The verb that resolved
    /// the workspace already answered this when it resolved a `Checkout`;
    /// an integration that wants the answer reads it here rather than
    /// re-deriving it by testing `workspace_root` for a marker file.
    pub container_kind: ContainerKind,

    /// The active project name.
    pub project: &'a ProjectName,

    /// Repo entries from the project's `rwv.toml`, as an ordered list of
    /// `(path, entry)` pairs. Sorted by `RepoPath` (matches the BTreeMap
    /// iteration order in the manifest). Integrations only iterate this
    /// field; no random-access lookups are needed.
    pub repos: Vec<(RepoPath, RepoEntry)>,

    /// Per-integration config from the `integrations:` key in `rwv.toml`.
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

    /// The project's `workweave:` config from `rwv.toml`, if any.
    ///
    /// Made visible so integrations can detect cross-section collisions
    /// (e.g. a name claimed by both `static-files.files` and
    /// `workweave.link`). Defaults to `None` for projects with no
    /// `workweave:` section. Integrations that don't care about workweave
    /// config should leave this untouched.
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

/// What an [`Issue`] reports, as a value a consumer can dispatch on.
///
/// The prose in [`Issue::message`] is for an operator to read. Anything a
/// machine needs to route or re-render a finding by belongs here, because a
/// sentence is not a thing another surface can re-word: sentences are matched
/// with substring tests that break the moment the wording improves.
///
/// [`IssueKind::MemberIncompatibility`] carries its whole observation rather
/// than a tag, for that reason — its `key`, `on_disk`, `required` and
/// `required_by` are facts the predicate established, and a renderer that has
/// to recover them from the sentence is parsing English.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueKind {
    /// The ecosystem CLI this integration drives is absent from `PATH`.
    ToolMissing,
    /// A file the integration owns is absent from the project directory.
    ManagedFileMissing,
    /// On-disk owned content diverges from what `activate()` would write, or
    /// is no longer consumable by the tool that owns it.
    ManagedFileDrift,
    /// The owned key or region is present without rwv's ownership marker —
    /// the user holds the pen, and auto-repair would discard their content.
    ManagedFileUserHeld,
    /// The weave-root symlink onto an owned file is absent, occupied by real
    /// content, or resolves somewhere other than the declaring project.
    Surfacing,
    /// `rwv.toml` asks for something the workspace cannot satisfy: a name two
    /// sections claim, a declared file that is not there, a member topology
    /// the ecosystem tool rejects.
    ConfigRejected,
    /// An `Ownership::DefaultOnly` value that is incompatible with what the
    /// members require. Carries the observation the predicate made.
    MemberIncompatibility(Box<crate::integrations::merge::MemberIncompatibility>),
    /// An integration's hook returned an error and the runner captured it so
    /// the remaining integrations could still run.
    IntegrationFailed,
    /// Raised by one of `rwv doctor`'s own scans rather than by an
    /// integration. The typed discriminant for these is
    /// [`crate::check::CheckViolation`]; this variant says only that the
    /// finding came in on the core channel.
    CoreFinding,
}

impl IssueKind {
    /// The tag `member-incompatibility` findings carry on operator surfaces.
    ///
    /// The one kind whose tag is published prose as well as a discriminant —
    /// `docs/reference/doctor-findings.md` keys the category by this word and
    /// the message opens with it — so it is minted here and read from here.
    pub const MEMBER_INCOMPATIBILITY: &'static str = "member-incompatibility";

    /// The stable kebab-case tag this kind travels under.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::ToolMissing => "tool-missing",
            Self::ManagedFileMissing => "managed-file-missing",
            Self::ManagedFileDrift => "managed-file-drift",
            Self::ManagedFileUserHeld => "managed-file-user-held",
            Self::Surfacing => "surfacing",
            Self::ConfigRejected => "config-rejected",
            Self::MemberIncompatibility(_) => Self::MEMBER_INCOMPATIBILITY,
            Self::IntegrationFailed => "integration-failed",
            Self::CoreFinding => "core-finding",
        }
    }
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
    /// What this finding is, independent of how the message words it.
    pub kind: IssueKind,
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

    /// The manifest filenames this integration detects member repos by — the
    /// complete argument set it passes to
    /// [`IntegrationContext::detect_repos_with_manifest`].
    ///
    /// The runner pre-computes one detection list per filename declared here
    /// across the integrations it is about to run. A filename an integration
    /// detects by but does not declare misses the cache and silently falls
    /// back to a live filesystem scan on every call; a filename declared here
    /// and detected by nobody is a cache slot no reader has.
    ///
    /// Ecosystem-tool output the integration generates but never scans for
    /// (`go.sum`, `Cargo.lock`) does not belong here — those are
    /// [`Integration::generated_files`].
    ///
    /// The default is empty: an integration whose member list comes from
    /// config rather than from the filesystem detects nothing.
    fn detection_manifests(&self) -> &[&str] {
        &[]
    }

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
    /// (e.g., `npm install`, `uv sync`, `cargo fetch`) that follow membership
    /// changes. Fires whenever the workspace's set of active repos may have
    /// changed; users can suppress with `rwv activate --no-install`.
    ///
    /// **A hook materializes; it never moves a pin.** Its mandate is to make
    /// the ecosystem state implied by current membership and the pins already
    /// recorded real on disk — adding what membership requires, never
    /// re-resolving what a lockfile already fixes. Advancing a dependency is
    /// operator intent expressed through an operator verb, not a side effect
    /// of activation. An implementation that cannot honour that must not run
    /// here: a hook fires on paths the operator did not ask for a dependency
    /// update on (`rwv doctor --fix` among them), and firing frequency is
    /// only harmless because a materializing hook is idempotent.
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
    /// `activate()` would produce for the current `rwv.toml` + `rwv.lock`.
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
    /// `rwv.toml` + `rwv.lock`.
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
