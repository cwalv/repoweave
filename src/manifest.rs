//! Manifest types: `rwv.yaml` and `rwv.lock` parsing and representation.
//!
//! These types model the on-disk YAML format and the resolved in-memory
//! representation. Parsing produces a `Manifest`; locking produces a `LockFile`.

use crate::registry::RegistryName;
use crate::vcs::{RawRevisionId, RefName, ResolvedRevisionId};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Newtypes — distinguish semantically different strings at the type level
// ---------------------------------------------------------------------------

/// A local path relative to the workspace root (e.g., `github/chatly/server`).
///
/// ## Separator contract
///
/// `RepoPath` values are always forward-slash (`/`) separated, matching the
/// portable YAML convention described in the repoweave manifest spec.
/// Backslashes are rejected at every construction site — both at serde
/// deserialization and via [`RepoPath::new`] — so a manifest authored on
/// Windows (which might produce `github\acme\server`) is caught immediately
/// rather than silently mismatching the forward-slash paths written by
/// sync/fetch. This mirrors the approach Cargo uses for `Cargo.toml` — YAML
/// stays portable; conversion to native OS paths happens at
/// filesystem-boundary calls via [`RepoPath::as_path`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RepoPath(String);

/// Typed error returned by [`RepoPath::new`].
///
/// Each variant corresponds to a specific validation rule; callers can
/// `match` on the variant to distinguish failure modes without parsing
/// the error message string.
///
/// `From<RepoPathError> for anyhow::Error` is provided automatically by
/// `anyhow` because `RepoPathError` implements `std::error::Error + Send +
/// Sync + 'static`, so existing `?`-based call-chains in anyhow-returning
/// callers continue to work without any changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoPathError {
    /// The path contains a backslash (`\`).
    ///
    /// `RepoPath` values must use forward-slash (`/`) separators. On Windows,
    /// normalise with [`str::replace`] before calling `RepoPath::new`.
    Backslash(String),
}

impl fmt::Display for RepoPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backslash(s) => write!(
                f,
                "backslash not allowed in RepoPath '{s}'; \
                 use forward slash (e.g. `github/acme/server` not `github\\acme\\server`)"
            ),
        }
    }
}

impl std::error::Error for RepoPathError {}

/// Validate that a `RepoPath` string contains no backslashes.
///
/// Returns `Err(RepoPathError)` when validation fails.
/// Shared by [`RepoPath::new`] and the [`serde::Deserialize`] impl so both
/// paths produce the same error text.
fn validate_repo_path(s: &str) -> Result<(), RepoPathError> {
    if s.contains('\\') {
        Err(RepoPathError::Backslash(s.to_owned()))
    } else {
        Ok(())
    }
}

impl RepoPath {
    /// Construct a `RepoPath`, returning a [`RepoPathError`] if `s` fails validation.
    ///
    /// Currently the only rejection is a backslash in the path; all
    /// `RepoPath` values must use forward-slash separators.  Use
    /// [`RepoPath::as_path`] to convert to a native OS path at
    /// filesystem-boundary calls.
    ///
    /// `?` propagation into `anyhow::Result`-returning callers works
    /// automatically via `From<RepoPathError> for anyhow::Error`.
    pub fn new(s: impl Into<String>) -> Result<Self, RepoPathError> {
        let s = s.into();
        validate_repo_path(&s)?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for RepoPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        validate_repo_path(&s).map_err(serde::de::Error::custom)?;
        Ok(RepoPath(s))
    }
}

/// A project name, possibly multi-segment (e.g., `web-app` or `chatly/web-app`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectName(String);

impl ProjectName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<Path> for ProjectName {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

/// A workweave name (e.g., `agent-42`, `hotfix`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkweaveName(String);

impl WorkweaveName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkweaveName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// RepoUrl — a clone source parsed into structured data
// ---------------------------------------------------------------------------

/// A clone source string parsed into its constituent parts.
///
/// Parsing happens once at the boundary via [`FromStr`] / [`Deserialize`],
/// which walks the registry list (built-in and any future user registries)
/// and returns the first match. Downstream code dispatches on the variant
/// rather than re-parsing.
///
/// `Display` reconstructs the canonical clone URL or shorthand form. For
/// inputs the registry list does not recognise, [`RepoUrl::Unknown`]
/// preserves the raw string verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RepoUrl {
    /// `https://{host}/{owner}/{repo}.git` — host matched a domain registry.
    Https {
        registry: RegistryName,
        host: String,
        owner: String,
        repo: String,
    },
    /// `git@{host}:{owner}/{repo}.git` — SCP-style SSH, host matched a domain registry.
    Ssh {
        registry: RegistryName,
        host: String,
        owner: String,
        repo: String,
    },
    /// `file://{prefix}/{owner}/{repo}` — under a directory registry.
    File {
        registry: RegistryName,
        prefix: PathBuf,
        owner: String,
        repo: String,
    },
    /// `owner/repo` (no registry — defaults at resolve time) or
    /// `{registry}/{owner}/{repo}` (registry named).
    Shorthand {
        registry: Option<RegistryName>,
        owner: String,
        repo: String,
    },
    /// Anything not matched by a registry — full URL with unknown host,
    /// non-shorthand string, etc. The raw form survives here.
    Unknown(String),
}

/// Error returned by [`RepoUrl::from_str`].
///
/// Currently a placeholder — `FromStr` falls back to [`RepoUrl::Unknown`]
/// rather than failing, so this type is reserved for future stricter parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoUrlParseError(pub String);

impl fmt::Display for RepoUrlParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not parse '{}' as a repo URL or shorthand", self.0)
    }
}

impl std::error::Error for RepoUrlParseError {}

impl FromStr for RepoUrl {
    type Err = RepoUrlParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Walk built-in registries; first match wins.
        let registries = crate::registry::builtin_registries();
        for reg in &registries {
            if let Some(url) = reg.matches(s) {
                return Ok(url);
            }
        }
        // 2-part shorthand fallback (registry-less; defaults at resolve time).
        if let Some(url) = parse_two_part_shorthand(s) {
            return Ok(url);
        }
        // Anything else: preserve the raw string.
        Ok(RepoUrl::Unknown(s.to_owned()))
    }
}

fn parse_two_part_shorthand(s: &str) -> Option<RepoUrl> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }
    Some(RepoUrl::Shorthand {
        registry: None,
        owner: parts[0].to_owned(),
        repo: parts[1].to_owned(),
    })
}

impl RepoUrl {
    /// The registry that recognised this URL, when one did.
    pub fn registry(&self) -> Option<&RegistryName> {
        match self {
            Self::Https { registry, .. }
            | Self::Ssh { registry, .. }
            | Self::File { registry, .. } => Some(registry),
            Self::Shorthand {
                registry: Some(r), ..
            } => Some(r),
            Self::Shorthand { registry: None, .. } | Self::Unknown(_) => None,
        }
    }

    /// Owner and repo, when the parser could extract them.
    pub fn owner_repo(&self) -> Option<(&str, &str)> {
        match self {
            Self::Https { owner, repo, .. }
            | Self::Ssh { owner, repo, .. }
            | Self::File { owner, repo, .. }
            | Self::Shorthand { owner, repo, .. } => Some((owner, repo)),
            Self::Unknown(_) => None,
        }
    }

    /// Whether this represents a URL form passable to `git clone`.
    /// HTTPS, SSH, File are URLs; Shorthand is not. Unknown is decided
    /// by inspecting the raw string.
    pub fn is_url(&self) -> bool {
        match self {
            Self::Https { .. } | Self::Ssh { .. } | Self::File { .. } => true,
            Self::Shorthand { .. } => false,
            Self::Unknown(s) => s.contains("://") || s.starts_with("git@"),
        }
    }

    /// Canonical local path `{registry}/{owner}/{repo}` for variants where the
    /// registry is known. Returns `None` for [`Self::Shorthand`] without a
    /// registry and for [`Self::Unknown`].
    pub fn local_path(&self) -> Option<PathBuf> {
        let registry = self.registry()?;
        let (owner, repo) = self.owner_repo()?;
        Some(Path::new(registry.as_str()).join(owner).join(repo))
    }
}

/// Loose equivalence between two clone URL strings.
///
/// Returns `true` when both strings resolve to the same logical repo after
/// stripping trailing slashes, a `.git` suffix, and normalising `git@host:`
/// SCP-style URLs to `https://host/`. Used for the `fork`-role mismatch
/// warning where we only need to recognise "origin already points at the
/// source-of-record" — not a security-grade comparison.
pub fn clone_urls_equivalent(a: &str, b: &str) -> bool {
    normalize_clone_url(a) == normalize_clone_url(b)
}

fn normalize_clone_url(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    // SCP-style: git@host:owner/repo(.git) -> host/owner/repo
    let body = if let Some(rest) = s.strip_prefix("git@") {
        rest.replacen(':', "/", 1)
    } else if let Some(rest) = s.strip_prefix("https://") {
        rest.to_owned()
    } else if let Some(rest) = s.strip_prefix("http://") {
        rest.to_owned()
    } else if let Some(rest) = s.strip_prefix("ssh://") {
        rest.trim_start_matches("git@").to_owned()
    } else if let Some(rest) = s.strip_prefix("file://") {
        rest.to_owned()
    } else {
        s.to_owned()
    };
    body.trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

impl fmt::Display for RepoUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Https {
                host, owner, repo, ..
            } => write!(f, "https://{}/{}/{}.git", host, owner, repo),
            Self::Ssh {
                host, owner, repo, ..
            } => write!(f, "git@{}:{}/{}.git", host, owner, repo),
            Self::File {
                prefix,
                owner,
                repo,
                ..
            } => write!(f, "file://{}/{}/{}", prefix.display(), owner, repo),
            Self::Shorthand {
                registry: None,
                owner,
                repo,
            } => write!(f, "{}/{}", owner, repo),
            Self::Shorthand {
                registry: Some(r),
                owner,
                repo,
            } => write!(f, "{}/{}/{}", r.as_str(), owner, repo),
            Self::Unknown(s) => f.write_str(s),
        }
    }
}

impl Serialize for RepoUrl {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RepoUrl {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<RepoUrl>().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Role — change-resistance level for a repo within a project
// ---------------------------------------------------------------------------

/// How freely code in this repo may be modified within the owning project.
///
/// **Naming.** The variant is spelled `owned` everywhere on the wire
/// (manifest YAML, `--role` CLI arguments, `--json` output). The legacy
/// `primary` spelling — used before the rename — is **not** accepted by
/// the parser; manifests carrying it must be migrated via `rwv doctor --fix`.
/// See `reference/roles.md` for the migration path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum Role {
    /// Your code. Change freely.
    Owned,
    /// Forked upstream. Changes ideally go upstream.
    Fork,
    /// Build dependency. Changes need upstream acceptance.
    Dependency,
    /// Read-only study material. No local changes.
    Reference,
}

impl Role {
    /// Whether this repo should appear in ecosystem workspace configs.
    /// Reference repos are excluded — they're not part of the build graph.
    pub fn is_active(&self) -> bool {
        !matches!(self, Role::Reference)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Owned => "owned",
            Role::Fork => "fork",
            Role::Dependency => "dependency",
            Role::Reference => "reference",
        }
    }
}

// ---------------------------------------------------------------------------
// Repo entry — one item in `repositories:`
// ---------------------------------------------------------------------------

/// The version control system backing a repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VcsType {
    Git,
    // Future: Jj, Sl, Hg
}

/// A single repo entry in an `rwv.yaml` manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntry {
    #[serde(rename = "type")]
    pub vcs_type: VcsType,
    pub url: RepoUrl,
    pub version: RefName,
    pub role: Role,
}

// ---------------------------------------------------------------------------
// Integration config — per-integration overrides in `rwv.yaml`
// ---------------------------------------------------------------------------

/// Per-integration configuration from the `integrations:` key.
///
/// Stored as a raw YAML mapping so each integration can define its own typed
/// settings struct without polluting a shared flat struct. The framework only
/// inspects the `enabled` key; all other keys are integration-specific.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntegrationConfig(serde_yaml::Mapping);

impl IntegrationConfig {
    /// Whether the integration should run.
    ///
    /// Returns `Some(true)` / `Some(false)` when `enabled:` is present in the
    /// YAML mapping, `None` when absent (fall back to `default_enabled()`).
    pub fn enabled(&self) -> Option<bool> {
        self.0
            .get(serde_yaml::Value::String("enabled".into()))
            .and_then(|v| v.as_bool())
    }

    /// Parse integration-specific settings into a typed struct.
    ///
    /// Returns `Err` if the YAML mapping cannot be deserialized into `T` so
    /// that callers can surface the parse error rather than silently falling
    /// back to a default.
    pub fn settings<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_yaml::Error> {
        serde_yaml::from_value(serde_yaml::Value::Mapping(self.0.clone()))
    }

    /// Convenience constructor: parse an `IntegrationConfig` from a YAML string.
    ///
    /// Useful in tests where you want to supply inline YAML rather than
    /// constructing a `serde_yaml::Mapping` by hand.
    ///
    /// # Panics
    /// Panics if the YAML is invalid or does not represent a mapping.
    pub fn from_yaml(yaml: &str) -> Self {
        serde_yaml::from_str(yaml).expect("IntegrationConfig::from_yaml: invalid YAML")
    }
}

// ---------------------------------------------------------------------------
// CargoWorkspaceConfig + MemberSpec — cargo-workspace integration settings
// ---------------------------------------------------------------------------

/// Sub-path specification for a single repo's contribution to the
/// weave-level Cargo workspace.
///
/// Repos listed in `CargoWorkspaceConfig::members` skip the root-as-member
/// auto-behavior and instead emit one member path per entry in `include` minus
/// `exclude`.
///
/// ## `include` default
///
/// An empty `include` means **no members from this repo** — the repo's root is
/// skipped and no sub-paths are added. This is the conservative default.
/// Operators must list every contributing sub-package explicitly. This matches
/// the rvtty scenario where the desired member list is
/// `[daemon, client, common]` — an absent include should never silently emit
/// an unintended root member.
///
/// ## `exclude`
///
/// Sub-paths listed here are removed from the effective include set. Useful
/// for omitting a sibling workspace directory (e.g. `workspace/`) from an
/// otherwise fully-listed include. Entries that do not appear in `include` are
/// silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemberSpec {
    /// Sub-paths under the repo root to add as workspace members.
    /// e.g. `["daemon", "client", "common"]`
    /// Empty → no members contributed by this repo.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,

    /// Sub-paths to remove from the effective include set.
    /// e.g. `["workspace"]` to skip a sibling-workspace directory.
    /// Entries not in `include` are silently ignored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// Per-integration configuration for the cargo-workspace integration.
///
/// Deserialized from the `integrations.cargo-workspace:` block in `rwv.yaml`
/// via `IntegrationConfig::settings::<CargoWorkspaceConfig>()`.
///
/// ## Design decisions locked in this type
///
/// **(a) Members sub-path config** — a repo listed in `members` contributes
/// the sub-paths from its `MemberSpec::include` list (minus `exclude`) instead
/// of its root.  Repos not in `members` keep the current root-as-member auto
/// behavior.
///
/// **(b) Merge-preserve scope** — rwv owns only `[workspace].members`,
/// `[workspace].resolver`, and (behind `workspace_package: true`)
/// `[workspace.package]`.  All other tables — `[profile.*]`,
/// `[workspace.dependencies]`, `[workspace.lints.*]`, `[workspace.metadata.*]`,
/// `[patch.*]` — are user policy and are never written or stripped by rwv.
///
/// **(c) Cross-repo path deps / publishability** — `patch: off` (default)
/// means all internal crates use committed relative `path=` deps. Two opt-in
/// modes let rwv generate `[patch]` entries:
///
/// - `patch: committed-paths` — mirrors committed cross-member path deps
///   into `[patch.crates-io]` (the pre-2026 behavior; also accepted as
///   `patch: true` for backward compat).
/// - `patch: derived` — matches each member's **registry** deps by crate
///   name against the member-path→package-name index and emits patch
///   entries directly. Sovereign members (publishable repos, `dependency`
///   / `fork` roles) get live in-weave sources without committing anything
///   weave-relative. Includes `reference`-role repos in the patchable
///   index (their symlinked directories keep logical paths). Git-source deps
///   emit `[patch."<url>"]` entries keyed by the dep's git URL rather than
///   by crates-io.
///
/// There is **no per-crate auto-detection**: rwv never reads or infers
/// `[package].publish` from member manifests.
///
/// **(d) Nested-workspace hard error** — repos with a `[workspace]` at their
/// root remain a hard activation error unless opted out via `exclude` or
/// listed under `members` (where the root is exempt from the workspace-check
/// because the sub-packages, not the root, are the declared members).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CargoWorkspaceConfig {
    /// Repo paths to exclude from the workspace entirely.
    ///
    /// Used as an escape hatch for repos with their own `[workspace]`
    /// declaration that cannot legally be nested.  Opted-out repos are
    /// surfaced as `# excluded: <path> (opted out)` comments in the
    /// generated file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,

    /// Per-repo member spec.
    ///
    /// When a repo path appears here, its root is **not** auto-treated as a
    /// workspace member.  Instead, members are emitted from
    /// `MemberSpec::include` minus `MemberSpec::exclude`.  Repos absent from
    /// this map keep the current root-as-member auto-behavior.
    ///
    /// Key format: repo path string as it appears in `rwv.yaml`
    /// (e.g. `"github/cwalv/rvtty"`).
    ///
    /// Example:
    /// ```yaml
    /// integrations:
    ///   cargo-workspace:
    ///     members:
    ///       github/cwalv/rvtty:
    ///         include: [daemon, client, common]
    ///         exclude: [fuzz]
    /// ```
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub members: BTreeMap<String, MemberSpec>,

    /// Cross-repo `[patch]` generation mode. Three shapes:
    ///
    /// - `PatchMode::Off` (the default; also accepted as `patch: false`):
    ///   rwv writes no `[patch]` entries. All internal crates use committed
    ///   relative `path=` dependencies.
    /// - `PatchMode::CommittedPaths` (also accepted as `patch: true`, the
    ///   pre-2026 shape): rwv mirrors each member's committed cross-member
    ///   path deps into `[patch.crates-io]` at the weave-root Cargo.toml.
    ///   The mirror keys off `path=` in member manifests.
    /// - `PatchMode::Derived`: rwv scans each member's **registry** deps
    ///   (`foo = "1.0"`, `foo = { version = "1.0" }`, `foo.workspace = true`)
    ///   and, when the crate name matches an in-weave package, emits a
    ///   patch entry. Sovereign members that declare `beads-core = "0.3"`
    ///   get patched to the in-weave fork without committing anything
    ///   weave-relative. Git-source deps produce `[patch."<url>"]` entries.
    ///   Version-incompatible in-weave crates are skipped (with a warning);
    ///   member `.cargo/config.toml` shadowing is surfaced at generation
    ///   time (see `CargoWorkspace::scan_patch_shadowing`).
    ///
    /// This is an **operator-selected** flag — rwv never auto-detects
    /// publishability from member `[package].publish` fields.
    /// The mode applies to the *entire weave*; there is no per-crate
    /// granularity.
    ///
    /// **Tier boundary** (documented, not asserted): under `derived`, a
    /// member depending on an *unpublished* crate name resolves only inside
    /// the weave — standalone the member fails with "no matching package".
    /// This is the tier boundary between fully-sovereign members and
    /// weave-native ones. Not a bug; document it where operators pick the
    /// mode.
    #[serde(default, skip_serializing_if = "PatchMode::is_off")]
    pub patch: PatchMode,

    /// **Where** rwv emits its `[patch.*]` entries. Orthogonal to
    /// [`Self::patch`] (which decides *what* patches to compute).
    ///
    /// - `PatchSurface::Manifest` (default): entries land in the managed
    ///   weave-root `Cargo.toml` under `[patch.*]`. Zero migration for
    ///   pre-2026 workspaces. Reaches every builder that consumes the
    ///   workspace manifest, but **cannot** reach nested-workspace member
    ///   builds (cargo hard-errors on nested workspace members; they opt
    ///   out of the weave workspace, and manifest `[patch]` is
    ///   workspace-scoped).
    /// - `PatchSurface::CargoConfig`: entries land in a generated
    ///   `.cargo/config.toml` alongside the managed `Cargo.toml`
    ///   (project dir; symlinked to the weave root). Config-level `[patch]`
    ///   is discovered *upward* from cwd, so nested-workspace opt-outs
    ///   (rvtty, mcp_agent_mail_rust — repos with their own `[workspace]`
    ///   root that cargo refuses to nest) *do* see the weave's patches
    ///   when built from inside their directory. This is the
    ///   nesting-immune surface.
    ///
    /// Enforced structurally as an enum — not two booleans — because
    /// emitting the same `[patch.<reg>].<crate>` key on *both* surfaces
    /// simultaneously is a cargo error (config-level `[patch]` shadows
    /// manifest `[patch]`, cargo warns about the manifest entry being
    /// unused, but a genuine key duplication with divergent paths would
    /// produce contradictory resolution). The enum makes the choice
    /// singular by construction.
    ///
    /// **Ignored when `patch == PatchMode::Off`** — there are no entries
    /// to route, so the surface choice is moot. The default (Manifest)
    /// stays inert.
    ///
    /// Caveat (documented in
    /// `docs/reference/integrations/cargo-workspace.md`): under
    /// `CargoConfig`, a member's own `.cargo/config.toml` `[patch.*]` key
    /// silently shadows the weave-level entry via cargo's
    /// closest-config-wins-per-key rule. Same shadowing surface as the
    /// existing `scan_patch_shadowing` observability axis;
    /// under `CargoConfig` the check remains equally load-bearing.
    #[serde(
        default,
        rename = "patch-surface",
        skip_serializing_if = "PatchSurface::is_manifest"
    )]
    pub patch_surface: PatchSurface,

    /// When `true`, rwv writes `[workspace.package]` from the project-level
    /// metadata declared in `rwv.yaml` (`project.license`, `project.authors`,
    /// etc.).  When `false` (default), `[workspace.package]` is left entirely
    /// to the user.
    ///
    /// Useful for single-product weaves where multiple publishable crates
    /// share project identity.  Not useful for multi-product weaves
    /// (cargo's `[workspace.package]` is workspace-wide with no per-member
    /// override) or for weaves of internal-only `publish = false` crates.
    #[serde(
        default,
        rename = "workspace-package",
        skip_serializing_if = "is_false"
    )]
    pub workspace_package: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

/// Cross-repo `[patch]` generation mode for the cargo-workspace integration.
///
/// See [`CargoWorkspaceConfig::patch`] for the semantics of each variant.
///
/// ## Backward-compatible serialization
///
/// The wire format accepts both the string spellings (`off`, `committed-paths`,
/// `derived`) and the pre-2026 boolean shape (`false` → `Off`,
/// `true` → `CommittedPaths`) via a hand-written [`serde::Deserialize`]
/// impl. Existing manifests with `patch: true` / `patch: false` keep parsing
/// unchanged — no `rwv doctor --fix` migration machinery is needed
/// (backward-compat at the parse level only).
///
/// Serialization goes out in the string form (`off` / `committed-paths` /
/// `derived`) so operators who write config back out via `--json` or a
/// round-trip see the modern spelling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PatchMode {
    /// No rwv-generated `[patch]` entries. Committed relative `path=` deps
    /// are the only cross-repo mechanism.
    #[default]
    Off,
    /// Mirror each member's committed cross-member `path=` deps into
    /// `[patch.crates-io]` at the weave-root `Cargo.toml`. Pre-2026 behavior.
    /// Accepted as `patch: true` for wire-format back-compat.
    CommittedPaths,
    /// Match each member's registry/git deps by name against the in-weave
    /// package-name index and emit `[patch.crates-io].<name>` (registry) or
    /// `[patch."<git-url>"].<name>` (git-source) entries.
    Derived,
}

impl PatchMode {
    /// True if this mode does not write any `[patch]` entries.
    /// Convenience predicate for `#[serde(skip_serializing_if)]`.
    pub fn is_off(&self) -> bool {
        matches!(self, PatchMode::Off)
    }

    /// True if this mode emits any `[patch]` entries (either mirror or
    /// derived). Used at activation time to gate the patch-emit branch.
    pub fn emits_patches(&self) -> bool {
        !self.is_off()
    }
}

impl<'de> Deserialize<'de> for PatchMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Untagged over (bool | string) via a visitor: this keeps the
        // wire-format back-compat with `patch: true` / `patch: false` while
        // adding the modern string spellings. A dedicated visitor is
        // clearer than #[serde(untagged)] on a wrapper because it lets us
        // reject unknown strings with a targeted error (typo detection).
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = PatchMode;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(
                    f,
                    "boolean (`true`/`false`) or one of \
                     \"off\", \"committed-paths\", \"derived\""
                )
            }

            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(if v {
                    PatchMode::CommittedPaths
                } else {
                    PatchMode::Off
                })
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                match v {
                    "off" => Ok(PatchMode::Off),
                    "committed-paths" => Ok(PatchMode::CommittedPaths),
                    "derived" => Ok(PatchMode::Derived),
                    other => Err(E::custom(format!(
                        "unknown patch mode `{other}` (expected `off`, \
                         `committed-paths`, `derived`, or a boolean)"
                    ))),
                }
            }

            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                self.visit_str(&v)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Where rwv emits its `[patch.*]` entries.
///
/// See [`CargoWorkspaceConfig::patch_surface`] for full semantics. The two
/// values are:
///
/// - `Manifest`: `[patch.*]` lands in the managed weave-root `Cargo.toml`.
///   Pre-2026 behavior; default.
/// - `CargoConfig`: `[patch.*]` lands in a generated
///   `.cargo/config.toml` alongside `Cargo.toml`. Reaches nested-workspace
///   opt-outs via cargo's upward config discovery.
///
/// Two surfaces at once is structurally impossible (enum, not two booleans)
/// because emitting the same patch key on both surfaces would be a
/// double-patch — config-level `[patch]` takes precedence over manifest-
/// level `[patch]` in cargo's resolution, but keeping both would drift
/// silently and confuse the strip pass. One surface at a time is the
/// invariant this type enforces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PatchSurface {
    /// Patch entries land in the managed weave-root `Cargo.toml`.
    #[default]
    Manifest,
    /// Patch entries land in a generated `.cargo/config.toml`
    /// alongside the managed `Cargo.toml`. Reaches nested-workspace
    /// opt-outs via upward config discovery.
    CargoConfig,
}

impl PatchSurface {
    /// True if the surface is the default (`Manifest`). Convenience for
    /// `#[serde(skip_serializing_if)]`.
    pub fn is_manifest(&self) -> bool {
        matches!(self, PatchSurface::Manifest)
    }

    /// True if the surface is the generated `.cargo/config.toml`.
    pub fn is_cargo_config(&self) -> bool {
        matches!(self, PatchSurface::CargoConfig)
    }
}

// ---------------------------------------------------------------------------
// GoWorkConfig — go-work integration settings
// ---------------------------------------------------------------------------

/// Per-integration configuration for the go-work integration.
///
/// Deserialized from the `integrations.go-work:` block in `rwv.yaml`
/// via `IntegrationConfig::settings::<GoWorkConfig>()`.
///
/// All fields are optional with sensible defaults so the integration works
/// with an absent config block (the common case).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GoWorkConfig {
    /// Explicit go version to write into the `go` directive of `go.work`.
    ///
    /// When `Some`, the fallback hand-edit path writes `go <version>` into
    /// the file (replacing any existing `go` line); the primary `go work edit`
    /// path passes `-go=<version>` to the tool.
    ///
    /// When `None` (default), the go line in an existing `go.work` is
    /// **never touched** by the fallback path — the operator's hand-authored
    /// version is preserved.  This fixes the bug where the old code would
    /// unconditionally downgrade a user's `go 1.26` to the computed maximum
    /// across member go.mod files.
    ///
    /// YAML key: `go-version` (hyphen, matching rwv.yaml naming conventions).
    #[serde(
        default,
        rename = "go-version",
        skip_serializing_if = "Option::is_none"
    )]
    pub go_version: Option<String>,
}

// ---------------------------------------------------------------------------
// WorkweaveConfig — artifact handling for workweaves
// ---------------------------------------------------------------------------

/// Configuration for workweave artifact handling.
/// Declares which gitignored artifacts should be copied or linked
/// when creating a workweave.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkweaveConfig {
    /// Paths to symlink from workweave to primary (shared state).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link: Vec<String>,

    /// Paths to copy from primary to workweave (local config).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copy: Vec<String>,
}

// ---------------------------------------------------------------------------
// Manifest — the parsed `rwv.yaml`
// ---------------------------------------------------------------------------

/// A parsed `rwv.yaml` file — the source of truth for a project's repos.
///
/// ## Accessor contract
///
/// Callers outside this module should use the accessor methods to traverse
/// the repository set rather than touching `repositories` directly:
///
/// - [`Manifest::iter_repo_paths`] — iterate over every [`RepoPath`] key.
/// - [`Manifest::get_entry`] — look up a single [`RepoEntry`] by path.
/// - [`Manifest::iter_entries`] — iterate over `(path, entry)` pairs.
/// - [`Manifest::len`] — number of repositories.
/// - [`Manifest::is_empty`] — true when there are no repositories.
///
/// The `repositories` field is `pub(crate)`; external callers must use the
/// accessor methods above.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub(crate) repositories: BTreeMap<RepoPath, RepoEntry>,
    #[serde(default)]
    pub integrations: BTreeMap<String, IntegrationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workweave: Option<WorkweaveConfig>,
}

impl Manifest {
    /// Iterate over every [`RepoPath`] in the manifest, in sorted order.
    ///
    /// This is the preferred accessor for callers that only need keys;
    /// use [`iter_entries`][Self::iter_entries] when you also need the
    /// corresponding [`RepoEntry`].
    pub fn iter_repo_paths(&self) -> impl Iterator<Item = &RepoPath> {
        self.repositories.keys()
    }

    /// Look up a single [`RepoEntry`] by its [`RepoPath`].
    ///
    /// Returns `None` when the path is not present in the manifest.
    pub fn get_entry(&self, path: &RepoPath) -> Option<&RepoEntry> {
        self.repositories.get(path)
    }

    /// Iterate over `(path, entry)` pairs in the manifest, in sorted order.
    ///
    /// Use this when you need both the key and the value; prefer
    /// [`iter_repo_paths`][Self::iter_repo_paths] when only the path is
    /// needed, and [`get_entry`][Self::get_entry] for random access.
    pub fn iter_entries(&self) -> impl Iterator<Item = (&RepoPath, &RepoEntry)> {
        self.repositories.iter()
    }

    /// Return the number of repositories in the manifest.
    ///
    /// Equivalent to `manifest.repositories.len()` but avoids direct field
    /// access. Prefer this over touching `repositories` directly.
    pub fn len(&self) -> usize {
        self.repositories.len()
    }

    /// Return `true` if the manifest contains no repositories.
    ///
    /// Equivalent to `manifest.repositories.is_empty()` but avoids direct
    /// field access. Prefer this over touching `repositories` directly.
    pub fn is_empty(&self) -> bool {
        self.repositories.is_empty()
    }

    /// Load from a YAML file.
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_yaml_str(&content)
            .with_context(|| format!("failed to parse rwv.yaml at {}", path.display()))
    }

    /// Parse a manifest from a YAML string, surfacing the
    /// legacy-`role: primary` migration hint when the parser rejects an
    /// otherwise-recognisable manifest.
    ///
    /// The back-compat alias on `role: primary` has been dropped; manifests
    /// using the legacy spelling now fail to parse. Detect that case up
    /// front and emit a pointer at `rwv doctor --fix` so users find the
    /// migration path instead of staring at a raw serde error.
    pub fn from_yaml_str(content: &str) -> anyhow::Result<Self> {
        match serde_yaml::from_str::<Self>(content) {
            Ok(manifest) => Ok(manifest),
            Err(err) => {
                if manifest_has_legacy_role_primary(content) {
                    Err(anyhow::anyhow!(
                        "manifest uses the deprecated `role: primary` spelling; \
                         run `rwv doctor --fix` to migrate to `role: owned`"
                    ))
                } else {
                    Err(err.into())
                }
            }
        }
    }
}

/// True iff `content` contains at least one `role: primary` line where
/// `primary` is the *full* value (not a prefix like `primary_repo`).
///
/// Used by the manifest loader and `rwv doctor` to detect the legacy
/// spelling now that its serde alias is gone. Targeted regex over raw
/// text avoids a full YAML round-trip, which would destroy comments and
/// key ordering when later rewriting the file under `--fix`.
pub fn manifest_has_legacy_role_primary(content: &str) -> bool {
    legacy_role_primary_regex().is_match(content)
}

/// Rewrite every `role: primary` in `content` to `role: owned`,
/// preserving surrounding whitespace, comments, and key ordering.
///
/// Idempotent: calling on a migrated manifest leaves it unchanged.
/// Returns `(new_content, replacements)`; `replacements` is `0` when no
/// legacy spellings were present.
pub fn migrate_legacy_role_primary(content: &str) -> (String, usize) {
    let re = legacy_role_primary_regex();
    let mut count = 0;
    let out = re
        .replace_all(content, |caps: &regex::Captures<'_>| {
            count += 1;
            // `$1` captures the indentation + `role:` + whitespace prefix.
            // `$2` captures the trailing character (whitespace, '#', or
            // newline) we need to preserve so that `role: primary  # foo`
            // and `role: primary\n` keep their original shape.
            format!("{}owned{}", &caps[1], &caps[2])
        })
        .into_owned();
    (out, count)
}

fn legacy_role_primary_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // (?m) — multiline so `^` matches line start.
    //
    // Capture 1: "<indent>role:<inter-token whitespace>" so the rewriter
    // can swap the value while keeping the line shape identical.
    //
    // Capture 2: the boundary character following `primary` —
    // whitespace, `#` (inline comment start), `\r`, or end-of-line. The
    // `regex` crate doesn't support lookaround, so we capture-and-emit
    // this character to fake the boundary; `primary_repo` and similar
    // identifiers fail to match because `_` isn't in the boundary set.
    //
    // End-of-input is matched separately via the `$` alternative which
    // consumes nothing (Capture 2 falls back to an empty match in that
    // case, which `replace_all` re-emits as empty).
    RE.get_or_init(|| {
        regex::Regex::new(r"(?m)^([ \t]*role:[ \t]+)primary([ \t#\r\n]|$)")
            .expect("legacy_role_primary regex compiles")
    })
}

// ---------------------------------------------------------------------------
// Lock file — pinned SHAs
// ---------------------------------------------------------------------------

/// A single entry in an `rwv.lock` file as parsed from YAML — version is
/// the raw string scalar; it has not yet been verified against any repo.
///
/// Use [`LockFile::resolve_versions`] to upgrade to [`ResolvedLockEntry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEntry {
    #[serde(rename = "type")]
    pub vcs_type: VcsType,
    pub url: RepoUrl,
    pub version: RawRevisionId,
}

/// A parsed `rwv.lock` file — entries carry raw, unresolved version
/// strings (tag/branch/SHA — unknown which). This is the parse-boundary
/// type: only [`LockFile::from_path`] / deserialization produces one.
/// To compare against a real commit SHA, run
/// [`LockFile::resolve_versions`] first.
///
/// ## Accessor contract
///
/// Callers outside this module should use the accessor methods to traverse
/// the repository set rather than touching `repositories` directly:
///
/// - [`LockFile::iter_repo_paths`] — iterate over every [`RepoPath`] key.
/// - [`LockFile::get_entry`] — look up a single [`LockEntry`] by path.
/// - [`LockFile::iter_entries`] — iterate over `(path, entry)` pairs.
/// - [`LockFile::len`] — number of repositories.
/// - [`LockFile::is_empty`] — true when there are no repositories.
/// - [`LockFile::contains_repo`] — test whether a path is present.
///
/// The `repositories` field is `pub(crate)`; external callers must use the
/// accessor methods above.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    /// Which workweave this lock was generated from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "weave")]
    pub workweave: Option<WorkweaveName>,
    pub(crate) repositories: BTreeMap<RepoPath, LockEntry>,
}

/// A single lock entry whose `version` has been resolved against a real
/// repo on disk and now carries the canonical commit SHA.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedLockEntry {
    #[serde(rename = "type")]
    pub vcs_type: VcsType,
    pub url: RepoUrl,
    pub version: ResolvedRevisionId,
}

/// A lock file whose entries are all resolved [`ResolvedLockEntry`]s.
///
/// There is no [`serde::Deserialize`] impl on purpose — the only way to
/// obtain a `ResolvedLockFile` is via [`LockFile::resolve_versions`] (or
/// via [`crate::lock::generate_lock`], which constructs entries from
/// [`crate::vcs::Vcs::head_revision`] return values that are already
/// canonical-SHA-form). This makes the parse/resolve boundary visible in
/// the type system.
///
/// ## Accessor contract
///
/// Callers outside this module should use the accessor methods to traverse
/// the repository set rather than touching `repositories` directly:
///
/// - [`ResolvedLockFile::iter_repo_paths`] — iterate over every [`RepoPath`] key.
/// - [`ResolvedLockFile::get_entry`] — look up a single [`ResolvedLockEntry`] by path.
/// - [`ResolvedLockFile::iter_entries`] — iterate over `(path, entry)` pairs.
/// - [`ResolvedLockFile::len`] — number of repositories.
/// - [`ResolvedLockFile::is_empty`] — true when there are no repositories.
/// - [`ResolvedLockFile::contains_repo`] — test whether a path is present.
///
/// The `repositories` field is `pub(crate)`; external callers must use the
/// accessor methods above.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedLockFile {
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "weave")]
    pub workweave: Option<WorkweaveName>,
    pub(crate) repositories: BTreeMap<RepoPath, ResolvedLockEntry>,
}

impl LockFile {
    /// Iterate over every [`RepoPath`] in the lock file, in sorted order.
    ///
    /// This is the preferred accessor for callers that only need keys;
    /// use [`iter_entries`][Self::iter_entries] when you also need the
    /// corresponding [`LockEntry`].
    pub fn iter_repo_paths(&self) -> impl Iterator<Item = &RepoPath> {
        self.repositories.keys()
    }

    /// Look up a single [`LockEntry`] by its [`RepoPath`].
    ///
    /// Returns `None` when the path is not present in the lock file.
    pub fn get_entry(&self, path: &RepoPath) -> Option<&LockEntry> {
        self.repositories.get(path)
    }

    /// Iterate over `(path, entry)` pairs in the lock file, in sorted order.
    ///
    /// Use this when you need both the key and the value; prefer
    /// [`iter_repo_paths`][Self::iter_repo_paths] when only the path is
    /// needed, and [`get_entry`][Self::get_entry] for random access.
    pub fn iter_entries(&self) -> impl Iterator<Item = (&RepoPath, &LockEntry)> {
        self.repositories.iter()
    }

    /// Return the number of repositories in the lock file.
    pub fn len(&self) -> usize {
        self.repositories.len()
    }

    /// Return `true` if the lock file contains no repositories.
    pub fn is_empty(&self) -> bool {
        self.repositories.is_empty()
    }

    /// Return `true` if the lock file contains an entry for `path`.
    pub fn contains_repo(&self, path: &RepoPath) -> bool {
        self.repositories.contains_key(path)
    }

    /// Return a reference to the underlying `BTreeMap` of repositories.
    ///
    /// Use this only when a `&BTreeMap<RepoPath, LockEntry>` is structurally
    /// required (e.g., index access `map[&key]`). Prefer
    /// [`iter_entries`][Self::iter_entries] or [`get_entry`][Self::get_entry]
    /// for all other access patterns.
    pub fn repo_map(&self) -> &BTreeMap<RepoPath, LockEntry> {
        &self.repositories
    }

    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_yaml_str(&content)
            .with_context(|| format!("failed to parse rwv.lock at {}", path.display()))
    }

    /// Parse a lock file from a YAML string.
    ///
    /// Used by snapshot reads (§6 of the sync design) where content is
    /// obtained via [`crate::vcs::Vcs::read_file_at_revision`] rather
    /// than from the working tree.
    pub fn from_yaml_str(content: &str) -> anyhow::Result<Self> {
        let lock: Self = serde_yaml::from_str(content)?;
        Ok(lock)
    }

    /// Consume the raw lock file and resolve each entry's `version`
    /// against its on-disk repo, producing a [`ResolvedLockFile`] whose
    /// entries carry canonical commit SHAs.
    ///
    /// Semantics (matches pre-split behaviour):
    /// - Repos missing on disk are silently skipped; their entries are
    ///   not present in the returned [`ResolvedLockFile`]. Callers that
    ///   need an "all entries" view should iterate the raw [`LockFile`]
    ///   before resolving (or keep a clone).
    /// - Repos whose `version` cannot be resolved by the local repo
    ///   (e.g. a tag or SHA the clone has never seen) are returned in
    ///   the failure list as `(RepoPath, RawRevisionId)` so callers can
    ///   surface the unresolved string in a diagnostic.
    ///
    /// Takes `self` (not `&mut self`) because the raw → resolved
    /// transformation is one-way: once an entry is resolved, the raw
    /// string would be misleading to keep around. Per
    /// `fp-principles-in-rust.md`, prefer transformations that return
    /// new immutable values.
    pub fn resolve_versions(
        self,
        workspace_dir: &Path,
    ) -> (ResolvedLockFile, Vec<(RepoPath, RawRevisionId)>) {
        let LockFile {
            workweave,
            repositories,
        } = self;
        let mut resolved = BTreeMap::new();
        let mut failures: Vec<(RepoPath, RawRevisionId)> = Vec::new();
        for (repo_path, entry) in repositories {
            let repo_abs = workspace_dir.join(repo_path.as_path());
            let LockEntry {
                vcs_type,
                url,
                version,
            } = entry;
            if !repo_abs.exists() {
                // Missing on disk: silently skip (pre-split behaviour).
                continue;
            }
            let vcs = crate::vcs::vcs_for(vcs_type);
            match vcs.resolve_revision(&repo_abs, version.as_str()) {
                Ok(resolved_rev) => {
                    resolved.insert(
                        repo_path,
                        ResolvedLockEntry {
                            vcs_type,
                            url,
                            version: resolved_rev,
                        },
                    );
                }
                Err(_) => failures.push((repo_path, version)),
            }
        }
        (
            ResolvedLockFile {
                workweave,
                repositories: resolved,
            },
            failures,
        )
    }
}

impl ResolvedLockFile {
    /// Iterate over every [`RepoPath`] in the resolved lock file, in sorted
    /// order.
    ///
    /// This is the preferred accessor for callers that only need keys;
    /// use [`iter_entries`][Self::iter_entries] when you also need the
    /// corresponding [`ResolvedLockEntry`].
    pub fn iter_repo_paths(&self) -> impl Iterator<Item = &RepoPath> {
        self.repositories.keys()
    }

    /// Look up a single [`ResolvedLockEntry`] by its [`RepoPath`].
    ///
    /// Returns `None` when the path is not present in the resolved lock file.
    pub fn get_entry(&self, path: &RepoPath) -> Option<&ResolvedLockEntry> {
        self.repositories.get(path)
    }

    /// Iterate over `(path, entry)` pairs in the resolved lock file, in
    /// sorted order.
    ///
    /// Use this when you need both the key and the value; prefer
    /// [`iter_repo_paths`][Self::iter_repo_paths] when only the path is
    /// needed, and [`get_entry`][Self::get_entry] for random access.
    pub fn iter_entries(&self) -> impl Iterator<Item = (&RepoPath, &ResolvedLockEntry)> {
        self.repositories.iter()
    }

    /// Return the number of repositories in the resolved lock file.
    pub fn len(&self) -> usize {
        self.repositories.len()
    }

    /// Return `true` if the resolved lock file contains no repositories.
    pub fn is_empty(&self) -> bool {
        self.repositories.is_empty()
    }

    /// Return `true` if the resolved lock file contains an entry for `path`.
    pub fn contains_repo(&self, path: &RepoPath) -> bool {
        self.repositories.contains_key(path)
    }

    /// Return a reference to the underlying `BTreeMap` of repositories.
    ///
    /// Use this only when a `&BTreeMap<RepoPath, ResolvedLockEntry>` is
    /// structurally required (e.g., index access `map[&key]`). Prefer
    /// [`iter_entries`][Self::iter_entries] or [`get_entry`][Self::get_entry]
    /// for all other access patterns.
    pub fn repo_map(&self) -> &BTreeMap<RepoPath, ResolvedLockEntry> {
        &self.repositories
    }
}

// ---------------------------------------------------------------------------
// Project — a resolved project on disk
// ---------------------------------------------------------------------------

/// A project directory with its manifest and optional lock file.
#[derive(Debug)]
pub struct Project {
    /// Path to the project directory (e.g., `projects/web-app/`).
    pub dir: PathBuf,
    pub name: ProjectName,
    pub manifest: Manifest,
    pub lock: Option<LockFile>,
}

impl Project {
    /// Derive a project name from a project directory path.
    ///
    /// Handles both relative and absolute paths by finding the `projects/`
    /// component anywhere in the path:
    ///
    /// - `projects/web-app`           → `"web-app"`
    /// - `projects/chatly/web-app`    → `"chatly/web-app"`
    /// - `/home/user/ws/projects/web-app` → `"web-app"`
    /// - `/home/user/ws/projects/chatly/web-app` → `"chatly/web-app"`
    ///
    /// Falls back to the last path component when no `projects/` ancestor is
    /// found (e.g., a bare temp dir used in tests).
    fn name_from_dir(dir: &Path) -> String {
        // Fast path: relative path starting with "projects/" (original behavior).
        if let Ok(rest) = dir.strip_prefix("projects") {
            return rest.to_string_lossy().into_owned();
        }

        // Absolute path: find the "projects" component and take everything after it.
        let components: Vec<_> = dir.components().collect();
        if let Some(idx) = components.iter().rposition(|c| c.as_os_str() == "projects") {
            let rest: PathBuf = components[idx + 1..].iter().collect();
            if !rest.as_os_str().is_empty() {
                return rest.to_string_lossy().into_owned();
            }
        }

        // Fallback: use the last component (e.g., bare temp dir without "projects").
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.to_string_lossy().into_owned())
    }

    /// Load a project from its directory.
    pub fn from_dir(dir: &Path) -> anyhow::Result<Self> {
        let manifest_path = dir.join("rwv.yaml");
        let manifest = Manifest::from_path(&manifest_path)
            .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;
        let lock_path = dir.join("rwv.lock");
        let lock = if lock_path.exists() {
            Some(
                LockFile::from_path(&lock_path)
                    .with_context(|| format!("failed to load lock at {}", lock_path.display()))?,
            )
        } else {
            None
        };

        // Derive project name from directory structure.
        // `projects/web-app/` → "web-app"
        // `projects/chatly/web-app/` → "chatly/web-app"
        // `/abs/path/projects/web-app/` → "web-app"
        let name = Self::name_from_dir(dir);

        Ok(Self {
            dir: dir.to_path_buf(),
            name: ProjectName::new(name),
            manifest,
            lock,
        })
    }

    /// Load a project from its directory without parsing `rwv.lock`.
    ///
    /// This is the recovery-path loader used by `rwv abort`. When a sync
    /// leaves the project repo in a mid-rebase state, `rwv.lock` may contain
    /// git conflict markers and fail strict YAML parsing. Abort only needs
    /// the project identity and manifest (to find repo paths); it never reads
    /// the lock. Using this variant makes that contract explicit so reviewers
    /// can see "this caller intentionally skips the lock".
    ///
    /// The returned `Project` always has `lock: None`, regardless of whether
    /// `rwv.lock` exists or what it contains.
    pub fn from_dir_skip_lock(dir: &Path) -> anyhow::Result<Self> {
        let manifest_path = dir.join("rwv.yaml");
        let manifest = Manifest::from_path(&manifest_path)
            .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

        // Derive project name from directory structure.
        // `projects/web-app/` → "web-app"
        // `projects/chatly/web-app/` → "chatly/web-app"
        // `/abs/path/projects/web-app/` → "web-app"
        let name = Self::name_from_dir(dir);

        Ok(Self {
            dir: dir.to_path_buf(),
            name: ProjectName::new(name),
            manifest,
            lock: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::vcs::{RefName, ResolvedRevisionId};

    // ========================================================================
    // IntegrationConfig — new transparent mapping API
    // ========================================================================

    #[derive(serde::Deserialize, Default, Debug, PartialEq)]
    struct TestSettings {
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        count: u32,
    }

    #[test]
    fn integration_config_default_is_empty_mapping() {
        let config = IntegrationConfig::default();
        assert!(config.enabled().is_none());
    }

    #[test]
    fn integration_config_enabled_some_true() {
        let config = IntegrationConfig::from_yaml("enabled: true");
        assert_eq!(config.enabled(), Some(true));
    }

    #[test]
    fn integration_config_enabled_some_false() {
        let config = IntegrationConfig::from_yaml("enabled: false");
        assert_eq!(config.enabled(), Some(false));
    }

    #[test]
    fn integration_config_enabled_absent_returns_none() {
        let config = IntegrationConfig::from_yaml("files: [foo.txt]");
        assert_eq!(config.enabled(), None);
    }

    #[test]
    fn integration_config_settings_deserializes_files_list() {
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [a.txt, b.txt]");
        let settings: TestSettings = config.settings().unwrap();
        assert_eq!(settings.files, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn integration_config_settings_returns_default_when_keys_missing() {
        let config = IntegrationConfig::from_yaml("enabled: true");
        let settings: TestSettings = config.settings().unwrap();
        assert_eq!(settings, TestSettings::default());
    }

    #[test]
    fn integration_config_settings_errors_on_wrong_type() {
        // `files` expects a sequence, but we supply a scalar — parse error surfaces.
        let config = IntegrationConfig::from_yaml("files: not-a-list");
        assert!(config.settings::<TestSettings>().is_err());
    }

    #[test]
    fn integration_config_arbitrary_keys_round_trip() {
        // IntegrationConfig should preserve unknown keys through serde.
        let yaml = "enabled: true\nfiles:\n  - x.json\ncount: 42\n";
        let config: IntegrationConfig = serde_yaml::from_str(yaml).unwrap();
        let restored = serde_yaml::to_string(&config).unwrap();
        let config2: IntegrationConfig = serde_yaml::from_str(&restored).unwrap();
        assert_eq!(config2.enabled(), Some(true));
        let settings: TestSettings = config2.settings().unwrap();
        assert_eq!(settings.files, vec!["x.json"]);
        assert_eq!(settings.count, 42);
    }

    #[test]
    fn integration_config_default_serializes_as_empty_mapping() {
        let config = IntegrationConfig::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        // Deserializing back should still give us an empty mapping
        let restored: IntegrationConfig = serde_yaml::from_str(&yaml).unwrap();
        assert!(restored.enabled().is_none());
    }

    // -- YAML test fixtures --------------------------------------------------

    const VALID_MANIFEST: &str = r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: owned
  github/acme/client:
    type: git
    url: https://github.com/acme/client.git
    version: develop
    role: fork
integrations:
  cargo:
    enabled: true
"#;

    const MINIMAL_MANIFEST: &str = r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: owned
"#;

    const VALID_LOCK: &str = r#"
workweave: hotfix-42
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: abc123def456
"#;

    const VALID_LOCK_NO_WORKWEAVE: &str = r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: abc123def456
"#;

    // ========================================================================
    // Role::is_active
    // ========================================================================

    #[test]
    fn role_owned_is_active() {
        assert!(Role::Owned.is_active());
    }

    // ========================================================================
    // clone_urls_equivalent
    // ========================================================================

    #[test]
    fn urls_equivalent_strip_dot_git() {
        assert!(clone_urls_equivalent(
            "https://github.com/a/b.git",
            "https://github.com/a/b"
        ));
    }

    #[test]
    fn urls_equivalent_ssh_vs_https() {
        assert!(clone_urls_equivalent(
            "git@github.com:a/b.git",
            "https://github.com/a/b.git"
        ));
    }

    #[test]
    fn urls_equivalent_trailing_slash() {
        assert!(clone_urls_equivalent(
            "https://github.com/a/b/",
            "https://github.com/a/b"
        ));
    }

    #[test]
    fn urls_equivalent_different_repos() {
        assert!(!clone_urls_equivalent(
            "https://github.com/a/b.git",
            "https://github.com/a/c.git"
        ));
    }

    #[test]
    fn role_fork_is_active() {
        assert!(Role::Fork.is_active());
    }

    #[test]
    fn role_dependency_is_active() {
        assert!(Role::Dependency.is_active());
    }

    #[test]
    fn role_reference_is_not_active() {
        assert!(!Role::Reference.is_active());
    }

    // ========================================================================
    // Manifest::from_path — valid files
    // ========================================================================

    #[test]
    fn manifest_from_path_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rwv.yaml");
        std::fs::write(&path, VALID_MANIFEST).unwrap();

        let m = Manifest::from_path(&path).unwrap();
        assert_eq!(m.repositories.len(), 2);
        assert_eq!(m.integrations.len(), 1);
    }

    #[test]
    fn manifest_from_path_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rwv.yaml");
        std::fs::write(&path, MINIMAL_MANIFEST).unwrap();

        let m = Manifest::from_path(&path).unwrap();
        assert_eq!(m.repositories.len(), 1);
        assert!(m.integrations.is_empty());
    }

    // ========================================================================
    // Manifest::from_path — error cases
    // ========================================================================

    #[test]
    fn manifest_from_path_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = Manifest::from_path(&dir.path().join("nonexistent.yaml"));
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("failed to read") || msg.contains("nonexistent.yaml"),
            "expected read error, got: {msg}"
        );
    }

    #[test]
    fn manifest_from_path_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.yaml");
        std::fs::write(&path, "{{{{not yaml at all::::").unwrap();

        let result = Manifest::from_path(&path);
        assert!(result.is_err());
    }

    #[test]
    fn manifest_from_path_missing_repositories_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.yaml");
        std::fs::write(&path, "integrations: {}\n").unwrap();

        let result = Manifest::from_path(&path);
        assert!(
            result.is_err(),
            "should fail when 'repositories' is missing"
        );
    }

    #[test]
    fn manifest_from_path_wrong_role_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_role.yaml");
        std::fs::write(
            &path,
            r#"
repositories:
  foo:
    type: git
    url: https://example.com
    version: main
    role: nonexistent_role
"#,
        )
        .unwrap();

        let result = Manifest::from_path(&path);
        assert!(result.is_err(), "unknown role should cause a parse error");
    }

    #[test]
    fn manifest_from_path_missing_url_in_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_url.yaml");
        std::fs::write(
            &path,
            r#"
repositories:
  foo:
    type: git
    version: main
    role: owned
"#,
        )
        .unwrap();

        let result = Manifest::from_path(&path);
        assert!(result.is_err(), "missing url should cause a parse error");
    }

    // ========================================================================
    // LockFile::from_path — valid files
    // ========================================================================

    #[test]
    fn lock_from_path_with_workweave() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rwv.lock");
        std::fs::write(&path, VALID_LOCK).unwrap();

        let lock = LockFile::from_path(&path).unwrap();
        assert_eq!(lock.workweave, Some(WorkweaveName::new("hotfix-42")));
        assert_eq!(lock.repositories.len(), 1);
    }

    #[test]
    fn lock_from_path_without_workweave() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rwv.lock");
        std::fs::write(&path, VALID_LOCK_NO_WORKWEAVE).unwrap();

        let lock = LockFile::from_path(&path).unwrap();
        assert_eq!(lock.workweave, None);
        assert_eq!(lock.repositories.len(), 1);
    }

    // ========================================================================
    // LockFile::from_path — error cases
    // ========================================================================

    #[test]
    fn lock_from_path_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = LockFile::from_path(&dir.path().join("nope.lock"));
        assert!(result.is_err());
    }

    #[test]
    fn lock_from_path_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.lock");
        std::fs::write(&path, "not: [valid: yaml: {{").unwrap();

        let result = LockFile::from_path(&path);
        assert!(result.is_err());
    }

    #[test]
    fn lock_from_path_missing_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.lock");
        std::fs::write(&path, "workweave: test\n").unwrap();

        let result = LockFile::from_path(&path);
        assert!(result.is_err(), "lock without repositories should fail");
    }

    // ========================================================================
    // Serde round-trips
    // ========================================================================

    #[test]
    fn manifest_serde_round_trip() {
        let original: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        let yaml = serde_yaml::to_string(&original).unwrap();
        let restored: Manifest = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(original.repositories.len(), restored.repositories.len());
        for (key, orig) in &original.repositories {
            let rest = &restored.repositories[key];
            assert_eq!(orig.vcs_type, rest.vcs_type);
            assert_eq!(orig.url, rest.url);
            assert_eq!(orig.version, rest.version);
            assert_eq!(orig.role, rest.role);
        }
    }

    #[test]
    fn lock_serde_round_trip_with_workweave() {
        let original: LockFile = serde_yaml::from_str(VALID_LOCK).unwrap();
        let yaml = serde_yaml::to_string(&original).unwrap();
        let restored: LockFile = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(original.workweave, restored.workweave);
        assert_eq!(original.repositories.len(), restored.repositories.len());
        for (key, orig) in &original.repositories {
            let rest = &restored.repositories[key];
            assert_eq!(orig.vcs_type, rest.vcs_type);
            assert_eq!(orig.url, rest.url);
            assert_eq!(orig.version, rest.version);
        }
    }

    #[test]
    fn lock_round_trip_no_workweave_omits_key() {
        let original: LockFile = serde_yaml::from_str(VALID_LOCK_NO_WORKWEAVE).unwrap();
        let yaml = serde_yaml::to_string(&original).unwrap();
        assert!(
            !yaml.contains("workweave:"),
            "workweave key should be omitted via skip_serializing_if"
        );
        assert!(
            !yaml.contains("weave:"),
            "weave key should be omitted via skip_serializing_if"
        );
        let restored: LockFile = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(restored.workweave, None);
    }

    #[test]
    fn role_serde_round_trip_all_variants() {
        for role in [Role::Owned, Role::Fork, Role::Dependency, Role::Reference] {
            let yaml = serde_yaml::to_string(&role).unwrap();
            let restored: Role = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(role, restored);
        }
    }

    /// The back-compat alias on `role: primary` is gone. A bare `primary`
    /// scalar must no longer deserialize as `Role::Owned` — otherwise the
    /// doctor-fix migration path wouldn't trigger.
    #[test]
    fn role_primary_yaml_no_longer_deserializes() {
        assert!(
            serde_yaml::from_str::<Role>("primary").is_err(),
            "`primary` must not parse as Role"
        );
    }

    /// Loading a full manifest with `role: primary` must surface a
    /// migration hint pointing at `rwv doctor --fix`. Without this,
    /// users hitting a legacy manifest see only the raw serde error.
    #[test]
    fn role_primary_yaml_fails_to_parse_with_helpful_error() {
        let yaml = r#"
repositories:
  github/acme/lib:
    type: git
    url: https://example.com/acme/lib.git
    version: main
    role: primary
"#;
        let err = Manifest::from_yaml_str(yaml).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("rwv doctor --fix"),
            "legacy `role: primary` manifest should point users at `rwv doctor --fix`, got: {msg}"
        );
        assert!(
            msg.contains("role: primary") || msg.contains("`role: primary`"),
            "error should name the deprecated spelling, got: {msg}"
        );
    }

    #[test]
    fn manifest_has_legacy_role_primary_detects_canonical_line() {
        let yaml = "    role: primary\n";
        assert!(manifest_has_legacy_role_primary(yaml));
    }

    #[test]
    fn manifest_has_legacy_role_primary_ignores_prefix_match() {
        // `primary_repo` is not the legacy spelling — must not match.
        let yaml = "    role: primary_repo\n";
        assert!(!manifest_has_legacy_role_primary(yaml));
    }

    #[test]
    fn manifest_has_legacy_role_primary_accepts_trailing_comment() {
        let yaml = "    role: primary  # legacy spelling\n";
        assert!(manifest_has_legacy_role_primary(yaml));
    }

    #[test]
    fn migrate_legacy_role_primary_rewrites_to_owned() {
        let yaml = "repositories:\n  github/acme/lib:\n    role: primary\n";
        let (out, count) = migrate_legacy_role_primary(yaml);
        assert_eq!(count, 1);
        assert!(out.contains("role: owned"));
        assert!(!out.contains("role: primary"));
    }

    #[test]
    fn migrate_legacy_role_primary_is_idempotent() {
        let yaml = "    role: owned\n";
        let (out, count) = migrate_legacy_role_primary(yaml);
        assert_eq!(count, 0);
        assert_eq!(out, yaml);
    }

    #[test]
    fn migrate_legacy_role_primary_preserves_comments_and_order() {
        let yaml = "\
# header comment
repositories:
  github/acme/lib:
    type: git           # inline comment
    url: https://example.com/acme/lib.git
    version: main
    role: primary       # legacy
  github/acme/app:
    type: git
    url: https://example.com/acme/app.git
    version: main
    role: owned
";
        let (out, count) = migrate_legacy_role_primary(yaml);
        assert_eq!(count, 1);
        // Header comment retained.
        assert!(out.contains("# header comment"));
        // Inline comment after migration retained.
        assert!(out.contains("# legacy"));
        // Order preserved (lib appears before app).
        let lib_pos = out.find("github/acme/lib").unwrap();
        let app_pos = out.find("github/acme/app").unwrap();
        assert!(lib_pos < app_pos);
        // The lib entry now uses `owned`.
        assert!(out.contains("role: owned       # legacy"));
        // No stray `role: primary` left.
        assert!(!out.contains("role: primary"));
    }

    /// Serialization is one-way: `Role::Owned` writes as `owned`, never
    /// `primary`. New manifests produced by the tool stay on the canonical
    /// spelling.
    #[test]
    fn role_owned_serializes_as_owned_not_primary() {
        let yaml = serde_yaml::to_string(&Role::Owned).unwrap();
        assert_eq!(yaml.trim(), "owned");
    }

    #[test]
    fn vcs_type_serde_round_trip() {
        let yaml = serde_yaml::to_string(&VcsType::Git).unwrap();
        let restored: VcsType = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(VcsType::Git, restored);
    }

    // ========================================================================
    // RepoPath helpers
    // ========================================================================

    #[test]
    fn repo_path_as_path() {
        let rp = RepoPath::new("github/acme/server").expect("known-safe literal");
        assert_eq!(rp.as_path(), Path::new("github/acme/server"));
    }

    // ========================================================================
    // RepoPath deserialize — separator validation
    // ========================================================================

    /// A valid forward-slash path must deserialize without error.
    #[test]
    fn repo_path_deserialize_forward_slash_accepted() {
        let rp: RepoPath = serde_yaml::from_str("github/acme/server").unwrap();
        assert_eq!(rp.as_str(), "github/acme/server");
    }

    /// A path containing only a backslash must be rejected at parse time.
    #[test]
    fn repo_path_deserialize_backslash_rejected() {
        let result: Result<RepoPath, _> = serde_yaml::from_str("github\\acme\\server");
        assert!(result.is_err(), "backslash path must be rejected");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("backslash not allowed"),
            "error should mention 'backslash not allowed', got: {msg}"
        );
        assert!(
            msg.contains("forward slash"),
            "error should mention 'forward slash', got: {msg}"
        );
    }

    /// A mixed-slash path (forward and back) must also be rejected.
    #[test]
    fn repo_path_deserialize_mixed_slash_rejected() {
        let result: Result<RepoPath, _> = serde_yaml::from_str("github/acme\\server");
        assert!(result.is_err(), "mixed-slash path must be rejected");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("backslash not allowed"),
            "error should mention 'backslash not allowed', got: {msg}"
        );
    }

    /// Backslash rejection surfaces correctly when embedded as a YAML map key
    /// in a full manifest, so users get a clear error rather than a generic one.
    #[test]
    fn repo_path_deserialize_backslash_rejected_in_manifest() {
        let yaml = r#"
repositories:
  github\acme\server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: owned
"#;
        let result = Manifest::from_yaml_str(yaml);
        assert!(
            result.is_err(),
            "manifest with backslash key must be rejected"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("backslash not allowed"),
            "error should mention 'backslash not allowed', got: {msg}"
        );
    }

    /// An empty string is accepted — it is the degenerate case and current
    /// internal code never produces it; we preserve the prior (unchecked)
    /// behavior rather than adding a new restriction outside this spec's scope.
    #[test]
    fn repo_path_deserialize_empty_string_accepted() {
        let rp: RepoPath = serde_yaml::from_str("''").unwrap();
        assert_eq!(rp.as_str(), "");
    }

    /// Serialization of a RepoPath round-trips correctly through YAML.
    #[test]
    fn repo_path_serde_round_trip() {
        let rp = RepoPath::new("github/acme/server").expect("known-safe literal");
        let yaml = serde_yaml::to_string(&rp).unwrap();
        let restored: RepoPath = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(rp, restored);
    }

    // ========================================================================
    // RepoPath::new — strict constructor invariant
    // ========================================================================

    /// `RepoPath::new` with a valid forward-slash path succeeds.
    #[test]
    fn repo_path_new_forward_slash_accepted() {
        let result = RepoPath::new("github/acme/server");
        assert!(result.is_ok(), "forward-slash path must succeed");
        assert_eq!(result.unwrap().as_str(), "github/acme/server");
    }

    /// `RepoPath::new` with a pure-backslash path returns Err with a clear message.
    #[test]
    fn repo_path_new_backslash_rejected() {
        let result = RepoPath::new("github\\acme\\server");
        assert!(result.is_err(), "backslash path must be rejected by new()");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("backslash not allowed"),
            "error should mention 'backslash not allowed', got: {msg}"
        );
        assert!(
            msg.contains("forward slash"),
            "error should mention 'forward slash', got: {msg}"
        );
    }

    /// `RepoPath::new` with a mixed-slash path (both forward and back) also returns Err.
    #[test]
    fn repo_path_new_mixed_slash_rejected() {
        let result = RepoPath::new("github/acme\\server");
        assert!(
            result.is_err(),
            "mixed-slash path must be rejected by new()"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("backslash not allowed"),
            "error should mention 'backslash not allowed', got: {msg}"
        );
    }

    /// The error message from `RepoPath::new` and the serde Deserialize impl
    /// share the same wording — callers get a consistent diagnostic regardless
    /// of whether the value came from YAML or internal code.
    #[test]
    fn repo_path_new_and_serde_produce_same_error_wording() {
        let new_msg = format!("{}", RepoPath::new("foo\\bar").unwrap_err());
        let serde_result: Result<RepoPath, _> = serde_yaml::from_str("foo\\bar");
        let serde_msg = format!("{}", serde_result.unwrap_err());
        // Both messages must contain the canonical diagnostic phrase.
        assert!(
            new_msg.contains("backslash not allowed"),
            "new() error must mention 'backslash not allowed', got: {new_msg}"
        );
        assert!(
            serde_msg.contains("backslash not allowed"),
            "serde error must mention 'backslash not allowed', got: {serde_msg}"
        );
    }

    // ========================================================================
    // RepoPathError — typed error variants
    // ========================================================================

    /// `RepoPath::new` returns the typed `RepoPathError::Backslash` variant
    /// (not an opaque `anyhow::Error`) so callers can pattern-match.
    #[test]
    fn repo_path_error_backslash_variant_fires() {
        let err = RepoPath::new("github\\acme\\server").unwrap_err();
        assert!(
            matches!(err, RepoPathError::Backslash(_)),
            "expected RepoPathError::Backslash, got: {err:?}"
        );
    }

    /// The `Backslash` variant carries the offending input string.
    #[test]
    fn repo_path_error_backslash_carries_input() {
        let input = "github\\acme\\server";
        let err = RepoPath::new(input).unwrap_err();
        let RepoPathError::Backslash(s) = &err;
        assert_eq!(
            s, input,
            "Backslash variant should carry the original input string"
        );
    }

    /// `RepoPathError` implements `std::error::Error`, meaning `anyhow` can
    /// convert it automatically via `?` in `anyhow::Result`-returning callers.
    #[test]
    fn repo_path_error_implements_std_error() {
        fn assert_std_error<E: std::error::Error>(_: &E) {}
        let err = RepoPath::new("foo\\bar").unwrap_err();
        assert_std_error(&err);
    }

    /// The Display of `RepoPathError::Backslash` includes the offending path
    /// so users know exactly which value triggered the error.
    #[test]
    fn repo_path_error_display_includes_offending_path() {
        let input = "win\\style\\path";
        let msg = format!("{}", RepoPath::new(input).unwrap_err());
        assert!(
            msg.contains(input),
            "Display should include the offending path '{input}', got: {msg}"
        );
    }

    // ========================================================================
    // Project::from_dir edge cases
    // ========================================================================

    #[test]
    fn project_from_dir_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let result = Project::from_dir(dir.path());
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("failed to load manifest"),
            "error should mention manifest: {msg}"
        );
    }

    #[test]
    fn project_from_dir_manifest_only_no_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();

        let project = Project::from_dir(dir.path()).unwrap();
        assert!(project.lock.is_none());
        assert_eq!(project.manifest.repositories.len(), 1);
        assert_eq!(project.dir, dir.path());
    }

    #[test]
    fn project_from_dir_bad_lock_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();
        std::fs::write(dir.path().join("rwv.lock"), "{{bad yaml").unwrap();

        let result = Project::from_dir(dir.path());
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("failed to load lock"),
            "error should mention lock: {msg}"
        );
    }

    // ========================================================================
    // Project::from_dir_skip_lock — lockless recovery-path loader
    // ========================================================================
    //
    // The strict loader (from_dir) must fail on all three failure modes;
    // the lockless loader must succeed on all three, returning lock: None.

    /// Conflict markers in rwv.lock are the primary symptom.
    /// from_dir fails; from_dir_skip_lock returns a usable project.
    #[test]
    fn project_from_dir_skip_lock_succeeds_with_conflict_markers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();
        let conflict_content = "\
<<<<<<< HEAD\nworkweave: hotfix\nrepositories: {}\n=======\nrepositories: {}\n>>>>>>> abc1234\n";
        std::fs::write(dir.path().join("rwv.lock"), conflict_content).unwrap();

        // Strict loader must fail.
        assert!(
            Project::from_dir(dir.path()).is_err(),
            "from_dir must fail when rwv.lock contains conflict markers"
        );

        // Lockless loader must succeed and return lock: None.
        let project = Project::from_dir_skip_lock(dir.path()).unwrap();
        assert!(
            project.lock.is_none(),
            "from_dir_skip_lock must return lock: None regardless of rwv.lock content"
        );
        assert_eq!(
            project.manifest.repositories.len(),
            1,
            "manifest should still be loaded"
        );
    }

    /// rwv.lock is missing entirely.
    /// from_dir succeeds with lock: None; from_dir_skip_lock also succeeds.
    #[test]
    fn project_from_dir_skip_lock_succeeds_with_missing_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();
        // No rwv.lock written.

        let project = Project::from_dir_skip_lock(dir.path()).unwrap();
        assert!(
            project.lock.is_none(),
            "from_dir_skip_lock must return lock: None when rwv.lock is absent"
        );
        assert_eq!(project.manifest.repositories.len(), 1);
    }

    /// rwv.lock exists but is empty (zero bytes).
    /// from_dir fails (empty YAML parses as null, which is not a valid LockFile struct);
    /// from_dir_skip_lock succeeds.
    #[test]
    fn project_from_dir_skip_lock_succeeds_with_empty_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();
        std::fs::write(dir.path().join("rwv.lock"), "").unwrap();

        // Strict loader must fail on an empty file.
        assert!(
            Project::from_dir(dir.path()).is_err(),
            "from_dir must fail when rwv.lock is empty"
        );

        // Lockless loader must succeed.
        let project = Project::from_dir_skip_lock(dir.path()).unwrap();
        assert!(
            project.lock.is_none(),
            "from_dir_skip_lock must return lock: None when rwv.lock is empty"
        );
        assert_eq!(project.manifest.repositories.len(), 1);
    }

    /// No rwv.yaml either — from_dir_skip_lock must still fail (manifest is required).
    #[test]
    fn project_from_dir_skip_lock_fails_without_manifest() {
        let dir = tempfile::tempdir().unwrap();
        // Neither rwv.yaml nor rwv.lock is present.

        let result = Project::from_dir_skip_lock(dir.path());
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("failed to load manifest"),
            "error should mention manifest: {msg}"
        );
    }

    #[test]
    fn project_name_from_projects_relative_path() {
        // Relative path starting with "projects/" — prefix is stripped.
        let name = Project::name_from_dir(Path::new("projects/my-app"));
        assert_eq!(name, "my-app");
    }

    #[test]
    fn project_name_nested_under_projects() {
        // Nested relative path — multi-segment name is preserved.
        let name = Project::name_from_dir(Path::new("projects/chatly/web-app"));
        assert_eq!(name, "chatly/web-app");
    }

    #[test]
    fn project_name_from_absolute_path_is_short_name() {
        // Absolute path: name_from_dir must return just the short name, not the
        // full absolute path. This is the regression guarded against here.
        let base = tempfile::tempdir().unwrap();
        let project_dir = base.path().join("projects").join("my-app");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();

        // Verify name_from_dir directly with the absolute path.
        let name = Project::name_from_dir(&project_dir);
        assert_eq!(
            name, "my-app",
            "absolute path should yield short project name, not full path"
        );

        // Also verify end-to-end via from_dir (which is what callers use).
        let project = Project::from_dir(&project_dir).unwrap();
        assert_eq!(
            project.name.as_str(),
            "my-app",
            "from_dir with absolute path must return short project name"
        );
    }

    #[test]
    fn project_name_nested_absolute_path_multi_segment() {
        // Absolute path with nested project (e.g., chatly/web-app under projects/).
        let base = tempfile::tempdir().unwrap();
        let project_dir = base.path().join("projects").join("chatly").join("web-app");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();

        let name = Project::name_from_dir(&project_dir);
        assert_eq!(
            name, "chatly/web-app",
            "absolute nested path should yield multi-segment project name"
        );

        let project = Project::from_dir(&project_dir).unwrap();
        assert_eq!(project.name.as_str(), "chatly/web-app");
    }

    #[test]
    fn project_name_from_dir_skip_lock_absolute_path() {
        // from_dir_skip_lock must also derive the correct short name.
        let base = tempfile::tempdir().unwrap();
        let project_dir = base.path().join("projects").join("my-service");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();

        let project = Project::from_dir_skip_lock(&project_dir).unwrap();
        assert_eq!(
            project.name.as_str(),
            "my-service",
            "from_dir_skip_lock with absolute path must return short project name"
        );
    }

    // ========================================================================
    // Empty-repos manifest
    // ========================================================================

    #[test]
    fn manifest_empty_repositories() {
        let yaml = "repositories: {}\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.repositories.is_empty());
        assert!(m.integrations.is_empty());
    }

    #[test]
    fn lock_empty_repositories() {
        let yaml = "repositories: {}\n";
        let lock: LockFile = serde_yaml::from_str(yaml).unwrap();
        assert!(lock.repositories.is_empty());
        assert_eq!(lock.workweave, None);
    }

    // ========================================================================
    // WorkweaveConfig serde
    // ========================================================================

    #[test]
    fn workweave_config_serde_round_trip() {
        let original = WorkweaveConfig {
            link: vec!["target/".to_string(), ".cargo/registry".to_string()],
            copy: vec![".env".to_string(), ".vscode/settings.json".to_string()],
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let restored: WorkweaveConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn manifest_with_workweave_section() {
        let yaml = r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: owned
workweave:
  link:
    - target/
  copy:
    - .env
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let ww = m.workweave.expect("workweave should be Some");
        assert_eq!(ww.link, vec!["target/"]);
        assert_eq!(ww.copy, vec![".env"]);
    }

    #[test]
    fn manifest_without_workweave_section() {
        let m: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        assert!(m.workweave.is_none());
    }

    #[test]
    fn lock_file_workweave_field() {
        let yaml = r#"
workweave: agent-42
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: abc123
"#;
        let lock: LockFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(lock.workweave, Some(WorkweaveName::new("agent-42")));
    }

    #[test]
    fn lock_file_weave_alias_backward_compat() {
        // Old lock files used `weave:` — the serde alias should read them.
        let yaml = r#"
weave: hotfix-99
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: deadbeef
"#;
        let lock: LockFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(lock.workweave, Some(WorkweaveName::new("hotfix-99")));
    }

    // ========================================================================
    // Project::from_dir with workweave in lock
    // ========================================================================

    #[test]
    fn project_from_dir_with_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rwv.yaml"), VALID_MANIFEST).unwrap();
        std::fs::write(dir.path().join("rwv.lock"), VALID_LOCK).unwrap();

        let project = Project::from_dir(dir.path()).unwrap();
        assert!(project.lock.is_some());
        let lock = project.lock.unwrap();
        assert_eq!(lock.workweave, Some(WorkweaveName::new("hotfix-42")));
    }

    // ========================================================================
    // Manifest accessor methods — iter_repo_paths / get_entry / iter_entries
    // ========================================================================

    // -- iter_repo_paths ------------------------------------------------------

    #[test]
    fn iter_repo_paths_empty_manifest() {
        let m: Manifest = serde_yaml::from_str("repositories: {}\n").unwrap();
        let paths: Vec<_> = m.iter_repo_paths().collect();
        assert!(paths.is_empty());
    }

    #[test]
    fn iter_repo_paths_single_repo() {
        let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST).unwrap();
        let paths: Vec<_> = m.iter_repo_paths().collect();
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0],
            &RepoPath::new("github/acme/server").expect("known-safe literal")
        );
    }

    #[test]
    fn iter_repo_paths_multi_repo_sorted() {
        // VALID_MANIFEST has two repos; BTreeMap keeps them in sorted order.
        let m: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        let paths: Vec<_> = m.iter_repo_paths().collect();
        assert_eq!(paths.len(), 2);
        // BTreeMap guarantees ascending key order.
        assert!(paths[0] < paths[1], "paths should be in sorted order");
        // Both repos present.
        let path_strs: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
        assert!(path_strs.contains(&"github/acme/server"));
        assert!(path_strs.contains(&"github/acme/client"));
    }

    // -- get_entry ------------------------------------------------------------

    #[test]
    fn get_entry_empty_manifest_returns_none() {
        let m: Manifest = serde_yaml::from_str("repositories: {}\n").unwrap();
        let result = m.get_entry(&RepoPath::new("github/acme/server").expect("known-safe literal"));
        assert!(result.is_none());
    }

    #[test]
    fn get_entry_present_returns_some() {
        let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST).unwrap();
        let entry = m.get_entry(&RepoPath::new("github/acme/server").expect("known-safe literal"));
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.role, Role::Owned);
        assert_eq!(entry.version, RefName::new("main"));
    }

    #[test]
    fn get_entry_absent_path_returns_none() {
        let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST).unwrap();
        let result =
            m.get_entry(&RepoPath::new("github/acme/nonexistent").expect("known-safe literal"));
        assert!(result.is_none());
    }

    #[test]
    fn get_entry_multi_repo_each_lookup() {
        let m: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        let server = m.get_entry(&RepoPath::new("github/acme/server").expect("known-safe literal"));
        let client = m.get_entry(&RepoPath::new("github/acme/client").expect("known-safe literal"));
        assert!(server.is_some());
        assert!(client.is_some());
        assert_eq!(server.unwrap().role, Role::Owned);
        assert_eq!(client.unwrap().role, Role::Fork);
    }

    // -- iter_entries ---------------------------------------------------------

    #[test]
    fn iter_entries_empty_manifest() {
        let m: Manifest = serde_yaml::from_str("repositories: {}\n").unwrap();
        let entries: Vec<_> = m.iter_entries().collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn iter_entries_single_repo() {
        let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST).unwrap();
        let entries: Vec<_> = m.iter_entries().collect();
        assert_eq!(entries.len(), 1);
        let (path, entry) = entries[0];
        assert_eq!(
            path,
            &RepoPath::new("github/acme/server").expect("known-safe literal")
        );
        assert_eq!(entry.role, Role::Owned);
    }

    #[test]
    fn iter_entries_multi_repo_all_present() {
        let m: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        let entries: Vec<_> = m.iter_entries().collect();
        assert_eq!(entries.len(), 2);
        // Paths reported by iter_entries must match iter_repo_paths.
        let paths_from_entries: Vec<&RepoPath> = entries.iter().map(|(p, _)| *p).collect();
        let paths_direct: Vec<&RepoPath> = m.iter_repo_paths().collect();
        assert_eq!(paths_from_entries, paths_direct);
    }

    #[test]
    fn iter_entries_consistent_with_get_entry() {
        // Every (path, entry) pair from iter_entries must agree with get_entry.
        let m: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        for (path, entry) in m.iter_entries() {
            let looked_up = m
                .get_entry(path)
                .expect("get_entry must find iter_entries path");
            // Compare a stable field to confirm it's the same entry.
            assert_eq!(entry.role, looked_up.role);
            assert_eq!(entry.version, looked_up.version);
        }
    }

    // -- len / is_empty -------------------------------------------------------

    #[test]
    fn len_empty_manifest() {
        let m: Manifest = serde_yaml::from_str("repositories: {}\n").unwrap();
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn len_single_repo() {
        let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST).unwrap();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn len_multi_repo() {
        let m: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn is_empty_empty_manifest() {
        let m: Manifest = serde_yaml::from_str("repositories: {}\n").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn is_empty_single_repo() {
        let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST).unwrap();
        assert!(!m.is_empty());
    }

    #[test]
    fn is_empty_multi_repo() {
        let m: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        assert!(!m.is_empty());
    }
}
