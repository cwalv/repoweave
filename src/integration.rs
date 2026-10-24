//! Integration trait and context.
//!
//! Integrations are pluggable units that derive config for one ecosystem tool
//! from the repo list. Each integration participates in activation (write path)
//! and check (read-only inspection).

use crate::manifest::{
    IntegrationConfig, Manifest, ProjectName, RepoEntry, RepoPath, WorkweaveConfig,
};
use crate::workspace::{project_dir, ContainerKind, WorkspaceSession};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Integration context — shared input for all integrations
// ---------------------------------------------------------------------------

/// What every integration in one run shares: the session's view of the weave,
/// narrowed to one project.
///
/// Built only by [`IntegrationContextBase::new`], which derives the project's
/// output directory instead of taking it.
pub struct IntegrationContextBase<'a> {
    session: &'a WorkspaceSession,
    output_dir: PathBuf,
    project: &'a ProjectName,
    detection_cache: &'a HashMap<String, Vec<String>>,
    workweave: Option<&'a WorkweaveConfig>,
}

impl<'a> IntegrationContextBase<'a> {
    /// `session` narrowed to `project`.
    ///
    /// The output directory is `<root>/projects/<project>`, where an
    /// integration's managed and generated files live, and is not a caller's
    /// choice: the weave root is the other directory a caller could hand over,
    /// and there the same files appear only as surfacing symlinks, for the
    /// active project alone. A context bound to the root reads the active
    /// project's inode whichever project it claims to describe, and names a
    /// path that does not exist for a file that is missing. Whether the root
    /// carries the symlink is [`crate::activate::verify_surfacing`]'s axis.
    ///
    /// The root, its container kind and the repos and projects on disk all
    /// come from `session`, which observed them together, so no caller can
    /// pair a root with a kind or a view taken from somewhere else.
    ///
    /// `detection_cache` is [`crate::integration_runner::build_detection_cache`]'s
    /// result for the integrations about to run; an empty cache makes every
    /// detection a live scan. `workweave` is the project's `workweave:`
    /// section, threaded through so an integration can see a name both it and
    /// `workweave.link` claim.
    pub fn new(
        session: &'a WorkspaceSession,
        project: &'a ProjectName,
        detection_cache: &'a HashMap<String, Vec<String>>,
        workweave: Option<&'a WorkweaveConfig>,
    ) -> Self {
        Self {
            session,
            output_dir: project_dir(session.root(), project),
            project,
            detection_cache,
            workweave,
        }
    }

    /// The directory the project's integration files live in:
    /// `<workspace_root>/projects/<project>`.
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    /// The weave root the session observed.
    pub fn workspace_root(&self) -> &'a Path {
        self.session.root()
    }

    pub fn project(&self) -> &'a ProjectName {
        self.project
    }

    /// The context one integration runs with: this base, `manifest`'s repos,
    /// and the integration's own `config`.
    pub fn build_context(
        &'a self,
        config: &'a IntegrationConfig,
        manifest: &'a Manifest,
    ) -> IntegrationContext<'a> {
        IntegrationContext {
            base: self,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config,
        }
    }
}

/// Everything an integration needs to do its work, read through accessors.
///
/// Built only by [`IntegrationContextBase::build_context`], once per
/// integration per run. Immutable — integrations read this, then write to the
/// filesystem.
pub struct IntegrationContext<'a> {
    base: &'a IntegrationContextBase<'a>,
    repos: Vec<(RepoPath, RepoEntry)>,
    config: &'a IntegrationConfig,
}

impl<'a> IntegrationContext<'a> {
    /// The directory an integration's managed and generated files live in:
    /// `<workspace_root>/projects/<project>`, in every weave and for every
    /// verb. Write hooks author here and read hooks inspect here, so a finding
    /// names the same path whichever verb produced it. Not the weave root:
    /// see [`IntegrationContextBase::new`].
    pub fn output_dir(&self) -> &'a Path {
        self.base.output_dir()
    }

    /// The weave root: the directory holding `projects/` and the registry
    /// dirs the repos live under, which is where manifest files inside repos
    /// are detected. The primary root at primary, the workweave directory
    /// inside one.
    pub fn workspace_root(&self) -> &'a Path {
        self.base.workspace_root()
    }

    /// Which kind of container [`Self::workspace_root`] is, as observed with
    /// it.
    pub fn container_kind(&self) -> ContainerKind {
        self.base.session.container_kind()
    }

    pub fn project(&self) -> &'a ProjectName {
        self.base.project
    }

    /// Repo entries from the project's `rwv.toml`, sorted by `RepoPath`.
    pub fn repos(&self) -> &[(RepoPath, RepoEntry)] {
        &self.repos
    }

    /// This integration's `[integrations.<name>]` block from `rwv.toml`.
    pub fn config(&self) -> &'a IntegrationConfig {
        self.config
    }

    /// Every repo on disk under the registry directories, as the session found
    /// it.
    pub fn all_repos_on_disk(&self) -> &'a [RepoPath] {
        self.base.session.repos_on_disk()
    }

    /// Every project under `projects/`, as the session found it.
    pub fn all_project_paths(&self) -> &'a [ProjectName] {
        self.base.session.project_paths()
    }

    /// The project's `workweave:` section from `rwv.toml`, if any.
    pub fn workweave(&self) -> Option<&'a WorkweaveConfig> {
        self.base.workweave
    }

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
            source: IssueSource::integration(integration),
            severity: Severity::Error,
            message: format!(
                "`[integrations.{integration}]` in rwv.toml does not parse into this \
                 integration's settings: {}. Correct the field it names, or drop it to take \
                 the default",
                format!("{e:#}").trim_end()
            )
            .into(),
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
        if let Some(cached) = self.base.detection_cache.get(filename) {
            return cached.clone();
        }
        detect_repos_with_manifest_impl(self.workspace_root(), &self.repos, filename)
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
        .filter(|(rp, _)| workspace_root.join(rp.as_path()).join(filename).exists())
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

/// Whether `rwv doctor --fix` may repair any finding of an [`IssueKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueFixScope {
    /// Decided per finding, by its [`Issue::safe_to_fix`].
    PerFinding,
    /// Never, whatever a finding's `safe_to_fix` says.
    Never,
}

impl IssueKind {
    /// The one place that says which issue kinds `rwv doctor --fix` may
    /// repair, read by its repair partition through [`Issue::fix_permitted`].
    /// The issues table's `--fix` column in
    /// `docs/reference/doctor-findings.md` is checked against it.
    ///
    /// `PerFinding` is necessary for a repair, not sufficient: a finding
    /// raised by a pass `--fix` never partitions is reported either way.
    pub fn fix_scope(&self) -> IssueFixScope {
        use IssueFixScope::{Never, PerFinding};
        match self {
            Self::ManagedFileMissing
            | Self::ManagedFileDrift
            | Self::Surfacing
            | Self::IntegrationFailed
            | Self::CoreFinding => PerFinding,
            Self::ToolMissing
            | Self::ManagedFileUserHeld
            | Self::ConfigRejected
            | Self::MalformedSettings
            | Self::MemberIncompatibility(_)
            | Self::DerivedStateStale
            | Self::DisabledIntegrationArtifact => Never,
        }
    }
}

impl Issue {
    /// Whether `rwv doctor --fix` may act on this finding: both its kind and
    /// the finding itself permit it.
    pub fn fix_permitted(&self) -> bool {
        self.safe_to_fix && self.kind.fix_scope() == IssueFixScope::PerFinding
    }
}

/// Where the write that produces a surfaced file lands, which is what decides
/// whether its weave-root symlink may exist before the file does.
///
/// [`SurfacedSource::WrittenThroughLink`] is the mechanism by which an
/// ecosystem tool's output reaches `projects/<project>/`: the tool runs at the
/// weave root, opens the declared name, and the kernel follows the symlink to
/// the canonical file. Take the link away and the tool creates a real file at
/// the weave root instead, where no repo tracks it; so does a tool that renames
/// a temporary file over a link that dangles. That real file is the tool's
/// output, and surfacing takes it into the project directory rather than
/// refusing on it (`crate::root_write`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfacedSource {
    /// A tool writes this path at the weave root. The link may precede the
    /// file, and a link over an absent file is the pending state rather than a
    /// stale one. A real file at the path is a write that missed the link.
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
/// Which weave's project checkout a surfaced link reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfacedCopy {
    /// The checkout under the weave root the link sits in.
    Weave,
    /// The primary's checkout, from every workweave of the project. State
    /// shared across workweaves lives there; at the primary itself the two
    /// copies are the same one.
    Primary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfacedFile {
    name: String,
    source: SurfacedSource,
    copy: SurfacedCopy,
}

impl SurfacedFile {
    /// Declare `name` as a path an ecosystem tool writes at the weave root.
    pub fn written_through_link(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: SurfacedSource::WrittenThroughLink,
            copy: SurfacedCopy::Weave,
        }
    }

    /// Declare `name` as a path that exists in the project directory before it
    /// is surfaced.
    pub fn written_at_source(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: SurfacedSource::WrittenAtSource,
            copy: SurfacedCopy::Weave,
        }
    }

    /// Read this path from the primary's project checkout rather than the
    /// weave's own.
    pub fn from_primary(self) -> Self {
        Self {
            copy: SurfacedCopy::Primary,
            ..self
        }
    }

    /// The path, relative to `output_dir`.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source(&self) -> SurfacedSource {
        self.source
    }

    pub fn copy(&self) -> SurfacedCopy {
        self.copy
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

/// Where a finding comes from: rwv's own checks, or one integration's hook.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IssueSource {
    Core,
    Integration(String),
}

impl IssueSource {
    pub fn integration(name: &str) -> Self {
        Self::Integration(name.to_string())
    }

    /// The name an operator reads and `--json` publishes: `core`, or the
    /// integration's own.
    pub fn name(&self) -> &str {
        match self {
            Self::Core => "core",
            Self::Integration(name) => name,
        }
    }
}

impl fmt::Display for IssueSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What an [`Issue`] says, which is only ever rendered: a finding is told
/// apart by its [`IssueKind`], never by reading its words back.
#[derive(Debug, Clone)]
pub struct IssueMessage(String);

impl From<String> for IssueMessage {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<&str> for IssueMessage {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}

impl fmt::Display for IssueMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A single issue reported by an integration's check or verify hook.
#[derive(Debug, Clone)]
pub struct Issue {
    pub source: IssueSource,
    pub severity: Severity,
    pub message: IssueMessage,
    /// What this finding is, independent of how the message words it.
    pub kind: IssueKind,
    /// This finding's own answer to whether `rwv doctor --fix` may repair it:
    /// `false` where the user holds the pen on a managed file region and an
    /// overwrite would destroy their content. Its kind can still forbid the
    /// repair; [`Issue::fix_permitted`] combines the two, and is what `--fix`
    /// and `--json` read.
    pub safe_to_fix: bool,
}

/// A CLI an integration shells out to, spelled once, where the integration
/// declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool(&'static str);

impl Tool {
    pub(crate) const fn new(name: &'static str) -> Self {
        Self(name)
    }

    pub fn name(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// An integration that shells out to one CLI. Its `PATH` probe, its install
/// hook and the [`HookOutcome::ToolMissing`] it reports all read [`Self::TOOL`],
/// and [`Integration::tool`] returns it for a caller holding only the trait
/// object.
pub trait DrivesTool: Integration {
    const TOOL: Tool;
}

/// A tool a hook looked for and did not find. Only an integration that
/// [`DrivesTool`] has one to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingTool(Tool);

impl MissingTool {
    pub fn of<I: DrivesTool>() -> Self {
        Self(I::TOOL)
    }

    pub fn tool(self) -> Tool {
        self.0
    }
}

/// What an [`Integration::activate_hook`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    /// Ran to completion, or had nothing to run.
    Done,
    /// The CLI the hook drives is not on `PATH`, so nothing ran.
    ToolMissing(MissingTool),
}

// ---------------------------------------------------------------------------
// Member incompatibility
// ---------------------------------------------------------------------------
//
// `Ownership::DefaultOnly` (`crate::integrations::merge::Ownership`)
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
// the `DefaultOnly` contract is untouched, `verify()` still reports
// `DefaultOnly` divergence as CLEAN, and the two coexist on the same file.
// Nothing gates on it — `doctor` and `update` report; neither refuses.

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
///    re-runs `activate()`, which by the `DefaultOnly` contract refuses to
///    overwrite an existing `DefaultOnly` value, so an automated repair does
///    not exist. A finding of this kind cannot be constructed claiming
///    otherwise.
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

/// The four strings an incompatibility is observed as, named so none stands
/// where another goes.
#[derive(Debug, Clone, Copy)]
pub struct Observed<'a> {
    /// Display form of the `DefaultOnly` key (e.g. `go`).
    pub key: &'a str,
    /// The value currently on disk.
    pub on_disk: &'a str,
    /// The strongest value the members require.
    pub required: &'a str,
    /// The member file carrying that requirement (e.g.
    /// `github/org/module-a/go.mod`).
    pub required_by: &'a str,
}

impl MemberIncompatibility {
    /// Record an incompatibility between a managed `DefaultOnly` value and the
    /// members' requirement.
    ///
    /// Every argument is an observation the predicate made from member files
    /// and the managed file. No wall-clock, no environment, no tooling probe —
    /// the category only carries statements that are true of the files on disk.
    pub fn new(integration: &dyn Integration, path: &Path, observed: Observed<'_>) -> Self {
        Self {
            integration: integration.name().to_string(),
            path: path.to_path_buf(),
            key: observed.key.to_string(),
            on_disk: observed.on_disk.to_string(),
            required: observed.required.to_string(),
            required_by: observed.required_by.to_string(),
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
        let kind = IssueKind::MemberIncompatibility(Box::new(self.clone()));
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
            tag = kind.tag(),
        );
        Issue {
            source: IssueSource::integration(&self.integration),
            severity: Severity::Warning,
            message: message.into(),
            kind,
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

    /// The CLI this integration shells out to, if it does. An integration that
    /// does implements [`DrivesTool`] and returns `Some(Self::TOOL)`.
    fn tool(&self) -> Option<Tool> {
        None
    }

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

    /// The keys this integration reads from its `[integrations.<name>]` table,
    /// besides the framework's own `enabled`. Parsing `rwv.toml` refuses any
    /// other key in that table, so an integration that reads settings returns
    /// [`settings_struct_keys`] of the type it deserializes them into.
    fn settings_keys(&self) -> &'static [&'static str] {
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
    /// changed; `--no-materialize` suppresses it on every verb that runs it.
    ///
    /// **A hook materializes; it never moves a pin.** Its mandate is to make
    /// the ecosystem state implied by current membership and the pins already
    /// recorded real on disk — adding what membership requires, never
    /// re-resolving what a lockfile already fixes. Advancing a dependency is
    /// operator intent expressed through an operator verb, not a side effect
    /// of activation. An implementation that cannot honor that must not run
    /// here: a hook fires on paths the operator did not ask for a dependency
    /// update on (`rwv doctor --fix` among them), and firing frequency is
    /// only harmless because a materializing hook is idempotent.
    ///
    /// Returns a [`HookOutcome`] rather than findings, unlike `activate()`, and
    /// the settings-shaped finding it therefore cannot return is kept away from
    /// it instead: the caller refuses every hook while any enabled
    /// integration's `[integrations.<name>]` block fails to deserialize, so an
    /// implementation reading its own settings here is reached only once they
    /// parse. A tool absent from `PATH` is an outcome, not an error: the hook
    /// ran nothing, and the runner reports it as [`IssueKind::ToolMissing`] at
    /// warning severity while the other integrations' hooks still run. What is
    /// left to bail with is a hook that failed while running, which reaches the
    /// operator as [`IssueKind::IntegrationFailed`] — whose advised repair,
    /// re-running the generators and the hooks, is the right one for that cause
    /// and the wrong one for a block nothing can read.
    fn activate_hook(&self, _ctx: &IntegrationContext) -> anyhow::Result<HookOutcome> {
        Ok(HookOutcome::Done)
    }

    /// Bring content whose right value is a fact about this host, not about
    /// `rwv.toml`, to this host's answer.
    ///
    /// Runs at every materialization — wherever the install hooks are not
    /// suppressed — ahead of surfacing, so a file this creates is linked in
    /// the same pass. The authoring verbs reach the same content through
    /// `activate()`, which owns everything the manifest decides; this owns
    /// only what a machine decides, so a context verb may run it without
    /// authoring anything the manifest asked for.
    fn fit_to_host(&self, _ctx: &IntegrationContext) -> anyhow::Result<()> {
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
    /// on-disk content match what `activate()` would write", and under the
    /// `DefaultOnly` contract a divergence is CLEAN there — permanently. This
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

/// The keys a derived settings struct accepts, as serde spells them on the
/// wire — renames applied — read off the field list serde hands a deserializer.
pub fn settings_struct_keys<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
    struct FieldListProbe<'a>(&'a mut &'static [&'static str]);

    impl<'de> serde::Deserializer<'de> for FieldListProbe<'_> {
        type Error = serde::de::value::Error;

        fn deserialize_any<V: serde::de::Visitor<'de>>(
            self,
            _: V,
        ) -> Result<V::Value, Self::Error> {
            Err(serde::de::Error::custom("not a struct"))
        }

        fn deserialize_struct<V: serde::de::Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            *self.0 = fields;
            Err(serde::de::Error::custom("field list read"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map enum identifier ignored_any
        }
    }

    let mut fields: &'static [&'static str] = &[];
    let _ = T::deserialize(FieldListProbe(&mut fields));
    fields
}

/// Whether an integration should run, considering its default and any override.
pub fn is_enabled(integration: &dyn Integration, config: &IntegrationConfig) -> bool {
    config.enablement().resolve(integration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_and_an_integration_named_core_are_different_sources() {
        assert_ne!(IssueSource::Core, IssueSource::integration("core"));
    }

    #[test]
    fn a_source_prints_as_the_name_an_operator_reads() {
        assert_eq!(IssueSource::Core.to_string(), "core");
        assert_eq!(
            IssueSource::integration("cargo-workspace").to_string(),
            "cargo-workspace"
        );
    }

    #[test]
    fn no_settings_struct_widens_its_keys_past_its_field_list() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![src.join("manifest.rs")];
        for entry in std::fs::read_dir(src.join("integrations")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
        let widening = ["flat", "ten"].concat();
        let renaming = ["ali", "as"].concat();
        let mut offenders = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for (start, _) in text.match_indices("#[serde(") {
                let attr = &text[start..start + text[start..].find(")]").unwrap_or(0)];
                if attr.contains(&widening) || attr.contains(&renaming) {
                    let line = text[..start].matches('\n').count() + 1;
                    offenders.push(format!("{}:{line}", file.display()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "settings_struct_keys reads the field list serde hands a deserializer; a key \
             accepted through either attribute is refused at parse: {offenders:?}"
        );
    }
}
