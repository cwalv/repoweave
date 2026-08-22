//! Integration trait and context.
//!
//! Integrations are pluggable units that derive config for one ecosystem tool
//! from the repo list. Each integration participates in activation (write path)
//! and check (read-only inspection).

use crate::manifest::{IntegrationConfig, ProjectName, RepoEntry, RepoPath, WorkweaveConfig};
use crate::workspace::ContainerKind;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    /// The typed settings for `integration`, or the finding to report when its
    /// `rwv.toml` block does not deserialize into them.
    ///
    /// A hook that propagates the parse error instead reaches the operator as
    /// `integration-failed`, which is the runner's capture of anything a hook
    /// bails with and names neither the field nor a remedy. Returning the
    /// finding is what keeps [`IssueKind::MalformedSettings`] on it — the
    /// deserializer's own message already names the field and the type it
    /// expected, so the caller has something to act on.
    ///
    /// `safe_to_fix` is false: the repair is an edit to `rwv.toml`, and on the
    /// `verify()` path a true here makes `--fix` attempt a regeneration whose
    /// input it has just failed to read.
    pub fn settings_or_issue<T: serde::de::DeserializeOwned>(
        &self,
        integration: &str,
    ) -> Result<T, Issue> {
        self.config.settings().map_err(|e| Issue {
            integration: integration.to_string(),
            severity: Severity::Error,
            message: format!(
                "`[integrations.{integration}]` in rwv.toml does not parse into this \
                 integration's settings: {}. Correct the field it names, or drop it to take \
                 the default",
                e.to_string().trim_end()
            ),
            kind: IssueKind::MalformedSettings,
            safe_to_fix: false,
        })
    }

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
    /// An `[integrations.<name>]` block does not deserialize into the settings
    /// type that integration declares. Distinct from
    /// [`IssueKind::ConfigRejected`], which is a well-formed request the
    /// workspace cannot meet: here no value was recovered, so nothing was
    /// asked and no predicate ran.
    MalformedSettings,
    /// An `Ownership::DefaultOnly` value that is incompatible with what the
    /// members require. Carries the observation the predicate made.
    MemberIncompatibility(Box<MemberIncompatibility>),
    /// Generated state whose attested inputs no longer describe the checkout.
    /// The condition `rwv sync` announces once, standing.
    DerivedStateStale,
    /// Content an integration authored is still on disk while that integration
    /// is disabled. Reported, never repaired: the implied state is absence, and
    /// reaching it means deleting.
    DisabledIntegrationArtifact,
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
            Self::MalformedSettings => "malformed-settings",
            Self::MemberIncompatibility(_) => Self::MEMBER_INCOMPATIBILITY,
            Self::DerivedStateStale => "derived-state-stale",
            Self::DisabledIntegrationArtifact => "disabled-integration-artifact",
            Self::IntegrationFailed => "integration-failed",
            Self::CoreFinding => "core-finding",
        }
    }
}

/// Where the write that produces a surfaced file lands, which is what decides
/// whether its weave-root symlink may exist before the file does.
///
/// [`SurfacedSource::WrittenThroughLink`] is the mechanism by which an
/// ecosystem tool's output reaches `projects/<project>/`: the tool runs at the
/// weave root, opens the declared name, and the kernel follows the symlink to
/// the canonical file. Take the link away and the tool creates a real file at
/// the weave root instead, where no repo tracks it and no later pass can see
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfacedSource {
    /// A tool writes this path at the weave root. The link may precede the
    /// file, and a link over an absent file is the pending state rather than a
    /// stale one.
    WrittenThroughLink,
    /// The file is in the project directory before the link is — rwv's own
    /// `activate()` put it there, or the operator committed it. The link
    /// follows the file, and one standing over an absent file is stale.
    WrittenAtSource,
}

/// One root-relative path an integration asks the framework to surface, and
/// where the write that produces it lands.
///
/// Constructed only through the two named constructors, so a declaration
/// cannot be written without answering the question. That is deliberate: the
/// wrong answer is silent and costs an untracked file at the weave root, which
/// is not a mistake a default should be able to make on an author's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfacedFile {
    name: String,
    source: SurfacedSource,
}

impl SurfacedFile {
    /// Declare `name` as a path an ecosystem tool writes at the weave root.
    pub fn written_through_link(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: SurfacedSource::WrittenThroughLink,
        }
    }

    /// Declare `name` as a path that exists in the project directory before it
    /// is surfaced.
    pub fn written_at_source(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: SurfacedSource::WrittenAtSource,
        }
    }

    /// The path, relative to `output_dir`.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source(&self) -> SurfacedSource {
        self.source
    }

    /// Consume the declaration into its two halves, for a consumer building an
    /// owned map keyed by path.
    pub fn into_parts(self) -> (String, SurfacedSource) {
        (self.name, self.source)
    }
}

/// One path an integration's ownership is written onto, and the cleanup shape
/// that ends it.
///
/// The distinction is what keeps cleanup from becoming data loss: removing a
/// file the operator co-owns would take their content with it, and stripping a
/// file rwv wrote whole would leave an empty shell behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedPath {
    /// rwv wrote the whole file. Cleanup is removal.
    WholeFile(String),
    /// rwv wrote a marked region inside a file the operator also holds.
    /// Cleanup is the integration's own strip, which keeps their content.
    MarkedRegion(String),
}

impl OwnedPath {
    /// The path, relative to `output_dir`.
    pub fn name(&self) -> &str {
        match self {
            Self::WholeFile(name) | Self::MarkedRegion(name) => name,
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
// Member incompatibility
// ---------------------------------------------------------------------------
//
// `Ownership::DefaultOnly` (rule 5, `crate::integrations::merge::Ownership`)
// conflates two predicates that only coincide at seed time:
//
// - PREFERENCE: divergence from *rwv's seeded default*. The user's business —
//   `verify()` is right to report CLEAN and say nothing.
// - INCOMPATIBILITY: divergence from *what the members require*. Not a
//   preference: the ecosystem toolchain rejects the configuration outright.
//   rwv computed the requirement (that is where the default came from) and can
//   see the breach.
//
// This category carries the second fact and nothing else. It is **not drift**:
// rule 5 is untouched, `verify()` still reports `DefaultOnly` divergence as
// CLEAN, and the two coexist on the same file. Nothing gates on it — `doctor`
// and `update` report; neither refuses.

/// An on-disk `Ownership::DefaultOnly` value that is incompatible with what
/// the workspace members require.
///
/// Constructed by an integration's `member_incompatibility` predicate (see
/// [`Integration::member_incompatibility`]) and converted to
/// an [`Issue`] by [`MemberIncompatibility::into_issue`] — the only route from
/// this type to a reportable finding.
///
/// # Why the fields are facts and the prose is not
///
/// The struct carries only *observations*; the message template lives here, in
/// Core. Two properties fall out of that and are not left to each construction
/// site to remember:
///
/// 1. **`safe_to_fix` is always `false`.** There is no field for it. `--fix`
///    re-runs `activate()`, which by rule-5 contract refuses to overwrite an
///    existing `DefaultOnly` value, so an automated repair does not exist. A
///    finding of this kind cannot be constructed claiming otherwise.
/// 2. **The message never advertises `--fix`.** It names the two remedies that
///    are actually available, both of which are the operator's: raise the
///    managed value, or lower the members' requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberIncompatibility {
    /// Integration name (goes into `Issue::integration`).
    integration: String,
    /// The managed file holding the incompatible value.
    path: PathBuf,
    /// Display form of the `DefaultOnly` key (e.g. `go`).
    key: String,
    /// The value currently on disk.
    on_disk: String,
    /// The strongest value the members require.
    required: String,
    /// Where that requirement comes from — the member file that carries it
    /// (e.g. `github/org/module-a/go.mod`). Named so the operator can go
    /// straight to the other end of the remedy.
    required_by: String,
}

impl MemberIncompatibility {
    /// Record an incompatibility between a managed `DefaultOnly` value and the
    /// members' requirement.
    ///
    /// Every argument is an observation the predicate made from member files
    /// and the managed file. No wall-clock, no environment, no tooling probe —
    /// the category only carries statements that are true of the files on disk.
    pub fn new(
        integration: &str,
        path: &Path,
        key: &str,
        on_disk: &str,
        required: &str,
        required_by: &str,
    ) -> Self {
        Self {
            integration: integration.to_string(),
            path: path.to_path_buf(),
            key: key.to_string(),
            on_disk: on_disk.to_string(),
            required: required.to_string(),
            required_by: required_by.to_string(),
        }
    }

    /// The managed file holding the incompatible value.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Display form of the `DefaultOnly` key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The value currently on disk.
    pub fn on_disk(&self) -> &str {
        &self.on_disk
    }

    /// The strongest value the members require.
    pub fn required(&self) -> &str {
        &self.required
    }

    /// The member file carrying that requirement.
    pub fn required_by(&self) -> &str {
        &self.required_by
    }

    /// Render this observation as the informational [`Issue`] both `rwv doctor`
    /// and `rwv update` surface.
    ///
    /// `safe_to_fix` is `false` and the message names both operator remedies;
    /// neither is a per-call-site choice (see the type docs). The observation
    /// itself rides on [`Issue::kind`], so a surface that wants the four facts
    /// reads them rather than parsing them back out of the sentence.
    pub fn into_issue(self) -> Issue {
        let message = format!(
            "{tag}: {} sets `{}` to `{}`, but the members \
             require `{}` (from {}) — the toolchain rejects this configuration. \
             rwv seeded this key once and never overwrites it, so this is not drift \
             and no automated repair applies: either raise `{}` to `{}` in {}, \
             or lower the requirement in {}.",
            self.path.display(),
            self.key,
            self.on_disk,
            self.required,
            self.required_by,
            self.key,
            self.required,
            self.path.display(),
            self.required_by,
            tag = IssueKind::MEMBER_INCOMPATIBILITY,
        );
        Issue {
            integration: self.integration.clone(),
            severity: Severity::Warning,
            message,
            kind: IssueKind::MemberIncompatibility(Box::new(self)),
            safe_to_fix: false,
        }
    }
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
    /// (`go.work.sum`, `Cargo.lock`) does not belong here — those are
    /// [`Integration::generated_files`].
    ///
    /// The default is empty: an integration whose member list comes from
    /// config rather than from the filesystem detects nothing.
    fn detection_manifests(&self) -> &[&str] {
        &[]
    }

    /// Generate config files and run install commands.
    /// Called during `rwv activate`, workweave creation, `rwv sync`, `rwv add`, and `rwv remove`.
    ///
    /// Returns findings the same way `check()` and `verify()` do — a
    /// condition with a published kind (a malformed `[integrations.<name>]`
    /// block, reported as `IssueKind::MalformedSettings`) is returned rather
    /// than bailed, so it reaches the operator under that kind instead of the
    /// runner's generic capture.
    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>>;

    /// Remove generated files. Called during deactivation.
    ///
    /// Stays `Result<()>`: cleanup acts on ownership evidence already on disk
    /// — a marker, a whole file — and reads no `[integrations.<name>]`
    /// settings, so there is no settings-shaped finding to misreport here.
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
    /// changed; users can suppress with `rwv activate --no-materialize`.
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
    ///
    /// Stays `Result<()>`, unlike `activate()`, and the settings-shaped finding
    /// it therefore cannot return is kept away from it instead: the caller
    /// refuses every hook while any enabled integration's
    /// `[integrations.<name>]` block fails to deserialize, so an implementation
    /// reading its own settings here is reached only once they parse. What is
    /// left to bail with is a hook that failed while running, which reaches the
    /// operator as [`IssueKind::IntegrationFailed`] — whose advised repair,
    /// re-running the generators and the hooks, is the right one for that cause
    /// and the wrong one for a block nothing can read.
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
    /// directory, and each declaration states where its own write lands (see
    /// [`SurfacedFile`]) — the two questions are independent, and a lockfile
    /// and an operator's committed file can both be fully owned while only one
    /// of them is written through its link.
    ///
    /// The default returns an empty list.
    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<SurfacedFile> {
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
    /// ownership semantics within itself. Where the two disagree about
    /// [`SurfacedSource`], `WrittenThroughLink` wins — suppressing a link a
    /// tool needs puts that tool's output where nothing tracks it, and
    /// keeping one that turns out permanently dangling costs an inert entry.
    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<SurfacedFile> {
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

    /// Where this integration's ownership is written under `output_dir`
    /// **right now**, and which cleanup shape ends each one. Read-only.
    ///
    /// The one hook that is meaningful for an integration that is **not
    /// enabled**. Every other read hook answers "does the on-disk state match
    /// what this integration would author", which a disabled integration has no
    /// answer to; this one answers "is anything of mine still here", which is
    /// exactly the question disablement raises. Nothing here consults
    /// membership or history — an artifact is attributed by the ownership
    /// evidence on disk, so no record of a previous enablement is needed.
    ///
    /// The set `deactivate` acts on, stated without acting. An entry here is a
    /// removal or a strip an operator can be asked to authorize, so an
    /// integration that returns a path it did not author is proposing to
    /// destroy someone else's file.
    ///
    /// **Default: empty**, and that is the safe answer rather than a stub.
    /// Declaring a file in [`Integration::generated_files`] means "symlink this
    /// from the weave root", not "rwv wrote this": `static-files` declares the
    /// operator's own committed files, and `go-work` declares a `go.work.sum`
    /// rwv has never authored a byte of. An integration that authors content
    /// overrides this and states its own ownership evidence.
    fn owned_paths_on_disk(&self, _ctx: &IntegrationContext) -> Vec<OwnedPath> {
        Vec::new()
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
    /// [`MemberIncompatibility`] for the category
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
    ) -> anyhow::Result<Option<MemberIncompatibility>> {
        Ok(None)
    }
}

/// Whether an integration should run, considering its default and any override.
pub fn is_enabled(integration: &dyn Integration, config: &IntegrationConfig) -> bool {
    config
        .enabled()
        .unwrap_or_else(|| integration.default_enabled())
}
