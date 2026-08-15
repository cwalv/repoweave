//! Manifest types: `rwv.toml` and `rwv.lock` parsing and representation.
//!
//! These types model the on-disk format and the resolved in-memory
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
/// portable convention described in the repoweave manifest spec.
/// Backslashes are rejected at every construction site — both at serde
/// deserialization and via [`RepoPath::new`] — so a manifest authored on
/// Windows (which might produce `github\acme\server`) is caught immediately
/// rather than silently mismatching the forward-slash paths written by
/// sync/fetch. This mirrors the approach Cargo uses for `Cargo.toml` — the
/// recorded path stays portable; conversion to native OS paths happens at
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

    /// Joining the result onto a native root (`root.join(repo_path.as_path())`)
    /// leaves the forward slashes in place, so on Windows the joined path
    /// mixes native and `/` separators. Win32 has accepted that mixed form on
    /// every filesystem call this crate's test suite has driven; other Win32
    /// API classes are untested against it.
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

/// The one member of the repo-path keyspace that is not a [`RepoPath`]: the
/// project repo, which every per-repo map and JSON record keys alongside the
/// manifest's own repos.
const PROJECT_REPO_KEY: &str = "(project)";

/// The key standing for the project repo where a [`RepoPath`] would sit.
///
/// The parenthesised spelling cannot collide with a `RepoPath`, which is a
/// relative directory path. Borrowed rather than owned: most sites are map
/// lookups, which want a `&str`.
pub(crate) fn project_repo_key() -> &'static str {
    PROJECT_REPO_KEY
}

pub use crate::naming::{ProjectName, ProjectNameError, WorkweaveName, WorkweaveNameError};

// ---------------------------------------------------------------------------
// RepoUrl — a clone source parsed into structured data
// ---------------------------------------------------------------------------

/// A clone source string parsed into its constituent parts.
///
/// Parsing happens once at the boundary via [`FromStr`] / [`Deserialize`],
/// which walks the registry list and returns the first match. Downstream
/// code dispatches on the variant rather than re-parsing.
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
            Self::Https { registry, .. } | Self::Ssh { registry, .. } => Some(registry),
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
            | Self::Shorthand { owner, repo, .. } => Some((owner, repo)),
            Self::Unknown(_) => None,
        }
    }

    /// Whether this represents a URL form passable to `git clone`.
    /// HTTPS and SSH are URLs; Shorthand is not. Unknown is decided
    /// by inspecting the raw string.
    pub fn is_url(&self) -> bool {
        match self {
            Self::Https { .. } | Self::Ssh { .. } => true,
            Self::Shorthand { .. } => false,
            Self::Unknown(s) => s.contains("://") || s.starts_with("git@"),
        }
    }

    /// Canonical local path `{registry}/{owner}/{repo}` for variants where the
    /// registry is known. Returns `None` for [`Self::Shorthand`] without a
    /// registry and for [`Self::Unknown`]. The value is the `RepoPath`
    /// identity spelling, `/`-separated on every platform.
    pub fn local_path(&self) -> Option<String> {
        let registry = self.registry()?;
        let (owner, repo) = self.owner_repo()?;
        Some(crate::registry::canonical_local_path(
            registry.as_str(),
            owner,
            repo,
        ))
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
/// (the manifest, `--role` CLI arguments, `--json` output). The legacy
/// `primary` spelling — used before the rename — is **not** accepted by
/// the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
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
    /// Every variant, in the order operator-facing text lists them.
    ///
    /// `--role` parsing and its error text are both derived from this slice,
    /// so a variant missing here is a variant the CLI cannot name. Nothing in
    /// the type system enforces completeness; `tests/role_single_mint_test.rs`
    /// does, by matching exhaustively.
    pub const ALL: &'static [Role] = &[Role::Owned, Role::Fork, Role::Dependency, Role::Reference];

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

    /// The spelling this role is no longer accepted under, and the sentence
    /// naming the one that replaced it.
    ///
    /// Manifest loading and `--role` parsing reach this through the same
    /// [`FromStr`], so an operator meets one sentence per migration rather
    /// than one per parser.
    pub const LEGACY_SPELLING: &'static str = "primary";

    pub fn legacy_spelling_hint() -> String {
        format!(
            "the `{legacy}` role spelling is no longer accepted; the role is \
             spelled `{owned}`",
            legacy = Self::LEGACY_SPELLING,
            owned = Role::Owned.as_str()
        )
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `--role` value did not name a [`Role`].
///
/// Carries the offending value so callers can name it; [`fmt::Display`] lists the
/// accepted spellings from [`Role::ALL`] rather than restating them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleParseError(pub String);

impl fmt::Display for RoleParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.eq_ignore_ascii_case(Role::LEGACY_SPELLING) {
            return f.write_str(&Role::legacy_spelling_hint());
        }
        let accepted: Vec<&str> = Role::ALL.iter().map(|r| r.as_str()).collect();
        write!(
            f,
            "'{}' is not a recognised role (expected {})",
            self.0,
            accepted.join(", ")
        )
    }
}

impl std::error::Error for RoleParseError {}

impl FromStr for Role {
    type Err = RoleParseError;

    /// Case-insensitive, over the same spellings [`Role::as_str`] writes.
    ///
    /// [`Role::LEGACY_SPELLING`] is deliberately absent: it must reach
    /// [`RoleParseError`] so the operator gets the migration sentence
    /// instead of a silent acceptance.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Role::ALL
            .iter()
            .copied()
            .find(|r| r.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| RoleParseError(s.to_owned()))
    }
}

/// Deserializes through [`FromStr`] rather than by derive, so that a manifest
/// naming [`Role::LEGACY_SPELLING`] is rejected by [`RoleParseError`] and
/// carries its migration sentence. A derived impl would answer with serde's
/// unknown-variant list, which names every accepted spelling but not the one
/// the operator actually typed.
impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
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

/// A single repo entry in an `rwv.toml` manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntry {
    #[serde(rename = "type")]
    pub vcs_type: VcsType,
    pub url: RepoUrl,
    pub version: RefName,
    pub role: Role,
}

// ---------------------------------------------------------------------------
// Integration config — per-integration overrides in `rwv.toml`
// ---------------------------------------------------------------------------

/// Per-integration configuration from the `[integrations]` table.
///
/// Stored as a raw TOML table so each integration can define its own typed
/// settings struct without polluting a shared flat struct. The framework only
/// inspects the `enabled` key; all other keys are integration-specific.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntegrationConfig(toml::Table);

impl IntegrationConfig {
    /// Whether the integration should run.
    ///
    /// Returns `Some(true)` / `Some(false)` when `enabled` is present in the
    /// table, `None` when absent (fall back to `default_enabled()`).
    pub fn enabled(&self) -> Option<bool> {
        self.0.get("enabled").and_then(|v| v.as_bool())
    }

    /// Parse integration-specific settings into a typed struct.
    ///
    /// Returns `Err` if the table cannot be deserialized into `T` so that
    /// callers can surface the parse error rather than silently falling back
    /// to a default.
    pub fn settings<T: serde::de::DeserializeOwned>(&self) -> Result<T, toml::de::Error> {
        toml::Value::Table(self.0.clone()).try_into()
    }

    /// Convenience constructor: parse an `IntegrationConfig` from a TOML string.
    ///
    /// Useful in tests where you want to supply inline TOML rather than
    /// constructing a [`toml::Table`] by hand.
    ///
    /// # Panics
    /// Panics if the TOML is invalid or does not represent a table.
    pub fn from_toml(toml_str: &str) -> Self {
        toml::from_str(toml_str).expect("IntegrationConfig::from_toml: invalid TOML")
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
/// Deserialized from the `integrations.cargo-workspace:` block in `rwv.toml`
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
    /// Key format: repo path string as it appears in `rwv.toml`
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
    /// metadata declared in `rwv.toml` (`project.license`, `project.authors`,
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
/// Deserialized from the `integrations.go-work:` block in `rwv.toml`
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
    /// Written `go-version`, hyphenated to match the manifest's key style.
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
// LockConfig — what `rwv lock` is allowed to record
// ---------------------------------------------------------------------------

/// Project-level policy for the revision form `rwv lock` records.
///
/// Policy rather than a flag on the verb: `rwv lock` also runs from inside
/// `rwv sync` and `rwv fetch`, and a lock whose form alternates with whoever
/// invoked it is worse than either form applied consistently.
///
/// `deny_unknown_fields` because a key here is set once and then trusted:
/// a misspelling accepted as "absent" would leave an operator believing a
/// guarantee they do not have.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockConfig {
    /// Record the commit id for every repo, including those a tag names.
    ///
    /// The name states what is surrendered. `rwv.lock` stops carrying the
    /// tag at HEAD, so it no longer reads as `v1.2.3` and no longer says
    /// which repos sit on a release. What that buys is a lock that
    /// reproduces the tree by itself: an entry records one revision and no
    /// SHA beside it, so a tag retargeted upstream moves what the lock
    /// resolves to, and nothing in the file says it moved.
    ///
    /// Whole-project by construction. The point is a lock that stands
    /// alone, and a lock standing alone for some of its entries does not.
    #[serde(
        default,
        rename = "forgo-tag-names",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub forgo_tag_names: bool,
}

// ---------------------------------------------------------------------------
// Manifest — the parsed `rwv.toml`
// ---------------------------------------------------------------------------

/// The accepted top-level keys of an `rwv.toml` manifest.
///
/// Listed once so that [`nearest_manifest_key`] and serde's
/// `deny_unknown_fields` error both name the same set. When a new top-level
/// table is added to [`Manifest`], add its TOML key here too.
pub(crate) const MANIFEST_TOP_LEVEL_KEYS: &[&str] =
    &["repositories", "integrations", "workweave", "lock"];

/// Return the element of [`MANIFEST_TOP_LEVEL_KEYS`] closest (by edit
/// distance) to `unknown`, or `None` when no element is within the threshold.
///
/// The threshold is `max(1, unknown.len() / 2)` to catch common typos
/// including transpositions, missing/extra characters, and single-character
/// substitutions.  This is intentionally simple — no external dependency,
/// no Unicode normalization; ASCII comparison is sufficient for TOML table
/// names.
fn nearest_manifest_key(unknown: &str) -> Option<&'static str> {
    /// Optimal String Alignment (OSA) distance — like Levenshtein but also
    /// counts adjacent transpositions as a single edit.  Strictly speaking
    /// OSA is not a true metric (the triangle inequality does not hold), but
    /// for short, human-typed keys the practical difference from full
    /// Damerau-Levenshtein is negligible and OSA is much simpler to
    /// implement without an external dependency.
    fn osa_distance(a: &str, b: &str) -> usize {
        let a = a.as_bytes();
        let b = b.as_bytes();
        if a.is_empty() {
            return b.len();
        }
        if b.is_empty() {
            return a.len();
        }
        // d[i][j] = OSA distance between a[..i] and b[..j].
        // We keep three rows: two-back (`prev2`), one-back (`prev`), current.
        let width = b.len() + 1;
        let mut prev2 = vec![0usize; width];
        let mut prev: Vec<usize> = (0..width).collect();
        let mut curr = vec![0usize; width];

        for (i, &ca) in a.iter().enumerate() {
            curr[0] = i + 1;
            for (j, &cb) in b.iter().enumerate() {
                let cost = if ca == cb { 0 } else { 1 };
                curr[j + 1] = (prev[j + 1] + 1) // deletion
                    .min(curr[j] + 1) // insertion
                    .min(prev[j] + cost); // substitution
                                          // Transposition: a[i-1]==b[j] && a[i]==b[j-1]
                if i > 0 && j > 0 && ca == b[j - 1] && a[i - 1] == cb {
                    curr[j + 1] = curr[j + 1].min(prev2[j - 1] + cost);
                }
            }
            // Rotate rows
            std::mem::swap(&mut prev2, &mut prev);
            std::mem::swap(&mut prev, &mut curr);
        }
        prev[b.len()]
    }

    // Threshold: accept matches within half the unknown key's length.
    // Min threshold is 1 so even a 1-char unknown gets a match on a known key
    // that differs by one edit.
    let threshold = (unknown.len() / 2).max(1);
    MANIFEST_TOP_LEVEL_KEYS
        .iter()
        .copied()
        .filter_map(|key| {
            let d = osa_distance(unknown, key);
            if d <= threshold {
                Some((d, key))
            } else {
                None
            }
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, key)| key)
}

/// A parsed `rwv.toml` file — the source of truth for a project's repos.
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
/// - [`Manifest::contains_repo`] — test whether a path is present.
/// - [`Manifest::insert_repo`] / [`Manifest::remove_repo`] /
///   [`Manifest::retain_repos`] — mutate the repository set.
/// - [`Manifest::write`] — serialize back to disk.
///
/// The `repositories` field is `pub(crate)`; external callers must use the
/// accessor methods above.
///
/// `deny_unknown_fields` because every top-level key is a policy table
/// (e.g. `[lock]`): a misspelling accepted as "absent" would leave an
/// operator believing a guarantee they do not have. The error names the
/// offending key and its line; [`Manifest::from_toml_str`] appends the
/// nearest accepted spelling so the operator can fix it immediately.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub(crate) repositories: BTreeMap<RepoPath, RepoEntry>,
    #[serde(default)]
    pub integrations: BTreeMap<String, IntegrationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workweave: Option<WorkweaveConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<LockConfig>,
}

/// Extract the offending key from a TOML `deny_unknown_fields` error message.
///
/// The toml crate formats these as:
/// `unknown field \`KEY\`, expected one of ...`
/// We pull out `KEY` so [`Manifest::from_toml_str`] can look up the
/// nearest valid spelling.
fn extract_unknown_key(msg: &str) -> Option<&str> {
    // Pattern: unknown field `KEY`, expected …
    let after = msg.find("unknown field `")?;
    let start = after + "unknown field `".len();
    let end = msg[start..].find('`')? + start;
    Some(&msg[start..end])
}

impl Manifest {
    /// The manifest's name inside a project directory.
    ///
    /// Public because operators type this name, `.gitattributes` entries
    /// carry it and documentation quotes it — it is an interface, not an
    /// implementation detail to be hidden. The constant exists so it is
    /// spelled once rather than re-typed at every path join.
    pub const FILE_NAME: &'static str = "rwv.toml";

    /// The name the manifest had before it became TOML.
    ///
    /// Nothing parses a file under this name. It exists so a project
    /// directory still holding one is refused by name rather than reported as
    /// having no manifest at all — see [`Self::legacy_format_refusal`].
    pub const LEGACY_FILE_NAME: &'static str = "rwv.yaml";

    /// The manifest text a project starts life with.
    pub const SKELETON: &'static str =
        "# Repoweave manifest — run `rwv add <url> --role <role>` to add repos.\n[repositories]\n";

    /// Whether `rwv lock` must record a commit id even where a tag names it.
    ///
    /// Absent `[lock]` table and absent key both mean no: the readable form
    /// is the default, and forgoing it is the thing an operator asks for.
    pub fn forgo_tag_names(&self) -> bool {
        self.lock.as_ref().is_some_and(|c| c.forgo_tag_names)
    }

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

    /// Return `true` if the manifest contains an entry for `path`.
    pub fn contains_repo(&self, path: &RepoPath) -> bool {
        self.repositories.contains_key(path)
    }

    /// Insert `entry` at `path`, returning whatever it displaced.
    pub fn insert_repo(&mut self, path: RepoPath, entry: RepoEntry) -> Option<RepoEntry> {
        self.repositories.insert(path, entry)
    }

    /// Remove the entry at `path`, returning it when one was present.
    pub fn remove_repo(&mut self, path: &RepoPath) -> Option<RepoEntry> {
        self.repositories.remove(path)
    }

    /// Drop every entry for which `keep` returns `false`.
    pub fn retain_repos(&mut self, keep: impl FnMut(&RepoPath, &mut RepoEntry) -> bool) {
        self.repositories.retain(keep);
    }

    /// The path a pre-TOML manifest would sit at, when one is there.
    ///
    /// `path` is the manifest path rwv looked for, so the legacy file is its
    /// sibling.
    pub fn legacy_beside(path: &Path) -> Option<PathBuf> {
        let legacy = path.with_file_name(Self::LEGACY_FILE_NAME);
        legacy.is_file().then_some(legacy)
    }

    /// The refusal for a project directory still holding a pre-TOML manifest.
    ///
    /// Report-only, and no `--fix` arm answers it: the file is hand-authored,
    /// so its comments and key order carry intent that no mechanical
    /// cross-format rewrite can place. Converting is the operator's, and
    /// saying so is the remedy rather than a gap.
    pub fn legacy_format_refusal(legacy_path: &Path) -> String {
        format!(
            "{legacy} is a YAML manifest; rwv reads {name}. Rewrite it as {name} by \
             hand and delete {legacy} — rwv will not convert it, because the comments \
             and key order you wrote cannot be carried across formats.",
            legacy = legacy_path.display(),
            name = Self::FILE_NAME
        )
    }

    /// Load from a TOML file.
    ///
    /// A project directory holding only the pre-TOML manifest is refused by
    /// name; without that arm it would read as having no manifest at all.
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                if let Some(legacy) = Self::legacy_beside(path) {
                    anyhow::bail!("{}", Self::legacy_format_refusal(&legacy));
                }
                return Err(anyhow::Error::new(err))
                    .with_context(|| format!("failed to read {}", path.display()));
            }
        };
        Self::from_toml_str(&content)
            .with_context(|| format!("failed to parse {} at {}", Self::FILE_NAME, path.display()))
    }

    /// The remedy for any `rwv.toml` parse failure.
    ///
    /// Mirrors [`LockFile::unparseable_hint`] and differs from it in the one
    /// way that matters to an operator: the lock is generated, so its remedy
    /// is to regenerate it, and this file is not, so its remedy is an edit.
    /// Neither has a `--fix` arm, and a reader who is told only that the file
    /// failed to parse cannot tell which of the two they are looking at.
    pub fn unparseable_hint() -> String {
        format!(
            "{} is yours to edit; rwv will not rewrite a file it could not parse",
            Self::FILE_NAME
        )
    }

    /// Parse a manifest from a TOML string.
    ///
    /// A rejected `role` value carries [`Role::legacy_spelling_hint`] through
    /// [`RoleParseError`], located at the offending line by the TOML parser.
    /// An unknown top-level key carries the offending key name, its line, and
    /// the nearest accepted spelling — the same quality of refusal
    /// [`LockConfig`] gives for a misspelled inner key.
    /// Any failure carries [`Self::unparseable_hint`] as its context, with the
    /// parser's own error kept as the source — the remedy is the same however
    /// the file broke, but only the parser can say where.
    pub fn from_toml_str(content: &str) -> anyhow::Result<Self> {
        toml::from_str(content).map_err(|e| {
            let msg = e.to_string();
            // When the error is an unknown top-level field, append a
            // "did you mean?" hint.  The TOML error already names the key and
            // its line; we add the nearest accepted spelling so the operator
            // can fix it in one read.
            let suggestion = extract_unknown_key(&msg)
                .and_then(nearest_manifest_key)
                .map(|nearest| format!("; did you mean `{nearest}`?"))
                .unwrap_or_default();
            let annotated = if suggestion.is_empty() {
                anyhow::Error::new(e)
            } else {
                // Wrap with the suggestion so it appears in the error chain
                // alongside the TOML location.
                anyhow::Error::new(e).context(format!(
                    "unknown top-level key in {}{}",
                    Self::FILE_NAME,
                    suggestion
                ))
            };
            annotated.context(Self::unparseable_hint())
        })
    }

    /// Serialize to TOML and write to `path`.
    ///
    /// The round-trip runs through serde, so any comment an operator wrote
    /// in the file being overwritten is gone afterwards — including the
    /// header [`Self::SKELETON`] starts a project with.
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        let text = toml::to_string(self).context("failed to serialize manifest")?;
        std::fs::write(path, &text)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Lock file — pinned SHAs
// ---------------------------------------------------------------------------

/// A single entry in an `rwv.lock` file as parsed from JSON — version is
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
/// - [`LockFile::insert_entry`] — add or replace one entry.
///
/// The `repositories` field is `pub(crate)`; external callers must use the
/// accessor methods above.
///
/// `deny_unknown_fields` rejects any key besides `repositories` — a lock
/// carrying a field this version no longer knows how to interpret is a
/// hand-edit or a lock from a different `rwv` era, and silently dropping
/// the key would make either indistinguishable from a clean parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockFile {
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
    pub(crate) repositories: BTreeMap<RepoPath, ResolvedLockEntry>,
}

impl ResolvedLockEntry {
    /// This entry in the raw form `rwv.lock` stores.
    ///
    /// The missing `Deserialize` above makes raw-to-resolved unreachable
    /// without resolving. The reverse is sound — a resolved SHA is already a
    /// well-formed raw version string — and lives here so that appending a
    /// freshly generated entry to an existing lock does not mean
    /// reconstructing a [`LockEntry`] field by field at the call site.
    pub fn to_raw(&self) -> LockEntry {
        LockEntry {
            vcs_type: self.vcs_type,
            url: self.url.clone(),
            version: crate::vcs::RawRevisionId::new(self.version.display_str()),
        }
    }
}

impl LockFile {
    /// The lock file's name inside a project directory.
    ///
    /// Public for the same reason [`Manifest::FILE_NAME`] is: operators name
    /// it in `.gitattributes` merge rules and in `git` invocations, so it is
    /// an interface. The constant keeps it spelled once.
    pub const FILE_NAME: &'static str = "rwv.lock";

    /// Insert `entry` at `path`, returning whatever it displaced.
    pub fn insert_entry(&mut self, path: RepoPath, entry: LockEntry) -> Option<LockEntry> {
        self.repositories.insert(path, entry)
    }

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
        Self::from_json_str(&content)
            .with_context(|| format!("failed to parse rwv.lock at {}", path.display()))
    }

    /// The remedy for any `rwv.lock` parse failure.
    ///
    /// The lock is fully derived state — see [`crate::lock::generate_lock`] —
    /// so an old YAML-era lock, truncation, and hand damage all have the same
    /// fix: regenerate it, rather than diagnose the specific cause.
    pub fn unparseable_hint() -> String {
        "rwv.lock could not be parsed; it is a generated file — run `rwv lock` to regenerate it"
            .to_string()
    }

    /// Parse a lock file from a JSON string.
    ///
    /// Used by snapshot reads, where content is obtained via
    /// [`crate::vcs::Vcs::read_file_at_revision`] rather than from the
    /// working tree. A parse failure carries [`Self::unparseable_hint`] as its
    /// context, with the serde error kept as the source: the remedy is the
    /// same for every cause, but a lock rwv itself just wrote failing to parse
    /// means regenerating it will not help, and then the cause is the only
    /// thing that tells you so.
    pub fn from_json_str(content: &str) -> anyhow::Result<Self> {
        serde_json::from_str(content)
            .map_err(|e| anyhow::Error::new(e).context(Self::unparseable_hint()))
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
    /// string would be misleading to keep around.
    pub fn resolve_versions(
        self,
        workspace_dir: &Path,
    ) -> (ResolvedLockFile, Vec<(RepoPath, RawRevisionId)>) {
        let LockFile { repositories } = self;
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
    /// Derive a project name from a project directory path, falling back to
    /// the last path component for a directory that is not sited in a weave
    /// (e.g., a bare temp dir used in tests).
    fn name_from_dir(dir: &Path) -> String {
        if let Some(name) = crate::workspace::project_name_from_dir(dir) {
            return name;
        }
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.to_string_lossy().into_owned())
    }

    /// Load a project from its directory.
    pub fn from_dir(dir: &Path) -> anyhow::Result<Self> {
        // Neither loader is wrapped here. Each already names its own file and
        // path, and a caller that reports this error has no other way to tell
        // which of the two files failed.
        let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
        let lock_path = dir.join(LockFile::FILE_NAME);
        let lock = if lock_path.exists() {
            Some(LockFile::from_path(&lock_path)?)
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
            name: ProjectName::new(name)?,
            manifest,
            lock,
        })
    }

    /// Load a project from its directory without parsing `rwv.lock`.
    ///
    /// This is the recovery-path loader used by `rwv abort`. When a sync
    /// leaves the project repo in a mid-rebase state, `rwv.lock` may contain
    /// git conflict markers and fail to parse. Abort only needs
    /// the project identity and manifest (to find repo paths); it never reads
    /// the lock. Using this variant makes that contract explicit so reviewers
    /// can see "this caller intentionally skips the lock".
    ///
    /// The returned `Project` always has `lock: None`, regardless of whether
    /// `rwv.lock` exists or what it contains.
    pub fn from_dir_skip_lock(dir: &Path) -> anyhow::Result<Self> {
        let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;

        // Derive project name from directory structure.
        // `projects/web-app/` → "web-app"
        // `projects/chatly/web-app/` → "chatly/web-app"
        // `/abs/path/projects/web-app/` → "web-app"
        let name = Self::name_from_dir(dir);

        Ok(Self {
            dir: dir.to_path_buf(),
            name: ProjectName::new(name)?,
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

    /// Deserialize a bare scalar through the manifest's codec.
    ///
    /// TOML has no bare-scalar document, so a newtype whose validation lives
    /// in its `Deserialize` is reached through a [`toml::Value`] rather than
    /// by parsing a one-line file.
    fn from_scalar<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, toml::de::Error> {
        toml::Value::String(s.to_owned()).try_into()
    }

    /// The inverse of [`from_scalar`], for round-trip assertions.
    fn to_scalar<T: Serialize>(value: &T) -> String {
        match toml::Value::try_from(value).unwrap() {
            toml::Value::String(s) => s,
            other => panic!("expected a string scalar, got {other:?}"),
        }
    }

    #[derive(serde::Deserialize, Default, Debug, PartialEq)]
    struct TestSettings {
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        count: u32,
    }

    #[test]
    fn integration_config_default_is_empty_table() {
        let config = IntegrationConfig::default();
        assert!(config.enabled().is_none());
    }

    #[test]
    fn integration_config_enabled_some_true() {
        let config = IntegrationConfig::from_toml("enabled = true");
        assert_eq!(config.enabled(), Some(true));
    }

    #[test]
    fn integration_config_enabled_some_false() {
        let config = IntegrationConfig::from_toml("enabled = false");
        assert_eq!(config.enabled(), Some(false));
    }

    #[test]
    fn integration_config_enabled_absent_returns_none() {
        let config = IntegrationConfig::from_toml("files = [\"foo.txt\"]");
        assert_eq!(config.enabled(), None);
    }

    #[test]
    fn integration_config_settings_deserializes_files_list() {
        let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\"a.txt\", \"b.txt\"]");
        let settings: TestSettings = config.settings().unwrap();
        assert_eq!(settings.files, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn integration_config_settings_returns_default_when_keys_missing() {
        let config = IntegrationConfig::from_toml("enabled = true");
        let settings: TestSettings = config.settings().unwrap();
        assert_eq!(settings, TestSettings::default());
    }

    #[test]
    fn integration_config_settings_errors_on_wrong_type() {
        // `files` expects a sequence, but we supply a scalar — parse error surfaces.
        let config = IntegrationConfig::from_toml("files = \"not-a-list\"");
        assert!(config.settings::<TestSettings>().is_err());
    }

    #[test]
    fn integration_config_arbitrary_keys_round_trip() {
        // IntegrationConfig should preserve unknown keys through serde.
        let text = "enabled = true\nfiles = [\"x.json\"]\ncount = 42\n";
        let config: IntegrationConfig = toml::from_str(text).unwrap();
        let restored = toml::to_string(&config).unwrap();
        let config2: IntegrationConfig = toml::from_str(&restored).unwrap();
        assert_eq!(config2.enabled(), Some(true));
        let settings: TestSettings = config2.settings().unwrap();
        assert_eq!(settings.files, vec!["x.json"]);
        assert_eq!(settings.count, 42);
    }

    #[test]
    fn integration_config_default_serializes_as_empty_table() {
        let config = IntegrationConfig::default();
        let text = toml::to_string(&config).unwrap();
        // Deserializing back should still give us an empty table
        let restored: IntegrationConfig = toml::from_str(&text).unwrap();
        assert!(restored.enabled().is_none());
    }

    // -- Manifest test fixtures ----------------------------------------------

    const VALID_MANIFEST: &str = r#"
[repositories."github/acme/server"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"

[repositories."github/acme/client"]
type = "git"
url = "https://github.com/acme/client.git"
version = "develop"
role = "fork"

[integrations.cargo]
enabled = true
"#;

    const MINIMAL_MANIFEST: &str = r#"
[repositories."github/acme/server"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"
"#;

    const VALID_LOCK: &str = r#"{
  "repositories": {
    "github/acme/server": {
      "type": "git",
      "url": "https://github.com/acme/server.git",
      "version": "abc123def456"
    }
  }
}
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
        let path = dir.path().join(Manifest::FILE_NAME);
        std::fs::write(&path, VALID_MANIFEST).unwrap();

        let m = Manifest::from_path(&path).unwrap();
        assert_eq!(m.repositories.len(), 2);
        assert_eq!(m.integrations.len(), 1);
    }

    #[test]
    fn manifest_from_path_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(Manifest::FILE_NAME);
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
        let path = dir.path().join("missing.toml");
        std::fs::write(&path, "[integrations]\n").unwrap();

        let result = Manifest::from_path(&path);
        assert!(
            result.is_err(),
            "should fail when 'repositories' is missing"
        );
    }

    #[test]
    fn manifest_from_path_wrong_role_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_role.toml");
        std::fs::write(
            &path,
            r#"
[repositories.foo]
type = "git"
url = "https://example.com"
version = "main"
role = "nonexistent_role"
"#,
        )
        .unwrap();

        let result = Manifest::from_path(&path);
        assert!(result.is_err(), "unknown role should cause a parse error");
    }

    #[test]
    fn manifest_from_path_missing_url_in_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_url.toml");
        std::fs::write(
            &path,
            r#"
[repositories.foo]
type = "git"
version = "main"
role = "owned"
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
    fn lock_from_path_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rwv.lock");
        std::fs::write(&path, VALID_LOCK).unwrap();

        let lock = LockFile::from_path(&path).unwrap();
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
    fn lock_from_path_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.lock");
        std::fs::write(&path, "not valid json {{").unwrap();

        let result = LockFile::from_path(&path);
        assert!(result.is_err());
    }

    /// The lock is fully derived state, so every parse failure — a
    /// pre-migration YAML lock, truncation, hand damage — has the same
    /// fix. The error must name it rather than surface a raw serde error.
    #[test]
    fn lock_from_path_parse_failure_names_regeneration_remedy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rwv.lock");
        std::fs::write(
            &path,
            "repositories:\n  github/acme/server:\n    version: abc123\n",
        )
        .unwrap();

        let err = LockFile::from_path(&path).unwrap_err();
        assert!(
            format!("{err:#}").contains("rwv lock"),
            "expected the regeneration remedy in the error chain, got: {err:#}"
        );
        assert!(
            err.chain().count() > 2,
            "the parse cause must survive under the remedy: a lock rwv itself \
             just wrote failing to parse means regenerating will not help, and \
             the cause is the only thing that says so. chain: {err:#}"
        );
    }

    #[test]
    fn lock_from_path_missing_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.lock");
        std::fs::write(&path, "{}\n").unwrap();

        let result = LockFile::from_path(&path);
        assert!(result.is_err(), "lock without repositories should fail");
    }

    /// A lock from before the `workweave` field was dropped must not parse
    /// as if the field were merely absent — `deny_unknown_fields` turns a
    /// stale key into a hard error rather than a silent drop.
    #[test]
    fn lock_from_path_rejects_legacy_workweave_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rwv.lock");
        std::fs::write(&path, r#"{"workweave": "hotfix-42", "repositories": {}}"#).unwrap();

        let result = LockFile::from_path(&path);
        assert!(
            result.is_err(),
            "a lock carrying the retired workweave key must be rejected, not silently accepted"
        );
    }

    // ========================================================================
    // Serde round-trips
    // ========================================================================

    #[test]
    fn manifest_serde_round_trip() {
        let original: Manifest = toml::from_str(VALID_MANIFEST).unwrap();
        let text = toml::to_string(&original).unwrap();
        let restored: Manifest = toml::from_str(&text).unwrap();

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
    fn lock_serde_round_trip() {
        let original: LockFile = serde_json::from_str(VALID_LOCK).unwrap();
        let json = serde_json::to_string(&original).unwrap();
        let restored: LockFile = serde_json::from_str(&json).unwrap();

        assert_eq!(original.repositories.len(), restored.repositories.len());
        for (key, orig) in &original.repositories {
            let rest = &restored.repositories[key];
            assert_eq!(orig.vcs_type, rest.vcs_type);
            assert_eq!(orig.url, rest.url);
            assert_eq!(orig.version, rest.version);
        }
    }

    #[test]
    fn role_serde_round_trip_all_variants() {
        for role in [Role::Owned, Role::Fork, Role::Dependency, Role::Reference] {
            let restored: Role = from_scalar(&to_scalar(&role)).unwrap();
            assert_eq!(role, restored);
        }
    }

    /// The back-compat alias on the legacy spelling is gone: it must not
    /// deserialize as `Role::Owned`, or a manifest carrying it would load
    /// silently under a role its author did not write.
    #[test]
    fn role_legacy_spelling_no_longer_deserializes() {
        assert!(
            from_scalar::<Role>(Role::LEGACY_SPELLING).is_err(),
            "`{}` must not parse as Role",
            Role::LEGACY_SPELLING
        );
    }

    /// A manifest carrying the legacy spelling is refused with the sentence
    /// naming the spelling that replaced it, located at the line that holds
    /// it. Nothing rewrites the file, so the sentence is the whole remedy and
    /// an operator who cannot find the offending line has not been given one.
    ///
    /// Rendered `{err:#}` because that is what surfaces this error to a
    /// person; `{err}` would read only [`Manifest::unparseable_hint`] and pass
    /// while the sentence below it never left the process.
    #[test]
    fn legacy_role_spelling_is_refused_with_the_replacement_named() {
        let text = r#"
[repositories."github/acme/lib"]
type = "git"
url = "https://example.com/acme/lib.git"
version = "main"
role = "primary"
"#;
        let err = Manifest::from_toml_str(text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&Role::legacy_spelling_hint()),
            "refusal should carry the migration sentence, got: {msg}"
        );
        assert!(
            msg.contains(Role::Owned.as_str()),
            "refusal should name the replacement spelling, got: {msg}"
        );
        assert!(
            msg.contains("line 6"),
            "refusal should locate the offending line, got: {msg}"
        );
    }

    /// Serialization is one-way: `Role::Owned` writes as `owned`, never
    /// `primary`. New manifests produced by the tool stay on the canonical
    /// spelling.
    #[test]
    fn role_owned_serializes_as_owned_not_primary() {
        assert_eq!(to_scalar(&Role::Owned), "owned");
    }

    #[test]
    fn vcs_type_serde_round_trip() {
        let restored: VcsType = from_scalar(&to_scalar(&VcsType::Git)).unwrap();
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
        let rp: RepoPath = from_scalar("github/acme/server").unwrap();
        assert_eq!(rp.as_str(), "github/acme/server");
    }

    /// A path containing only a backslash must be rejected at parse time.
    #[test]
    fn repo_path_deserialize_backslash_rejected() {
        let result: Result<RepoPath, _> = from_scalar("github\\acme\\server");
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
        let result: Result<RepoPath, _> = from_scalar("github/acme\\server");
        assert!(result.is_err(), "mixed-slash path must be rejected");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("backslash not allowed"),
            "error should mention 'backslash not allowed', got: {msg}"
        );
    }

    /// Backslash rejection surfaces correctly when embedded as a table key
    /// in a full manifest, so users get a clear error rather than a generic one.
    #[test]
    fn repo_path_deserialize_backslash_rejected_in_manifest() {
        let text = r#"
[repositories."github\\acme\\server"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"
"#;
        let result = Manifest::from_toml_str(text);
        assert!(
            result.is_err(),
            "manifest with backslash key must be rejected"
        );
        let msg = format!("{:#}", result.unwrap_err());
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
        let rp: RepoPath = from_scalar("").unwrap();
        assert_eq!(rp.as_str(), "");
    }

    /// Serialization of a RepoPath round-trips correctly.
    #[test]
    fn repo_path_serde_round_trip() {
        let rp = RepoPath::new("github/acme/server").expect("known-safe literal");
        let restored: RepoPath = from_scalar(&to_scalar(&rp)).unwrap();
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
    /// of whether the value came from the manifest or internal code.
    #[test]
    fn repo_path_new_and_serde_produce_same_error_wording() {
        let new_msg = format!("{}", RepoPath::new("foo\\bar").unwrap_err());
        let serde_result: Result<RepoPath, _> = from_scalar("foo\\bar");
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
    // ProjectName::new — parse-boundary validation
    // ========================================================================

    #[test]
    fn project_name_new_simple_accepted() {
        assert_eq!(ProjectName::new("web-app").unwrap().as_str(), "web-app");
    }

    /// The multi-segment shape `name_from_dir` derives for nested projects
    /// must keep working — `/` is not one of the rejected characters.
    #[test]
    fn project_name_new_multi_segment_slash_accepted() {
        assert_eq!(
            ProjectName::new("chatly/web-app").unwrap().as_str(),
            "chatly/web-app"
        );
    }

    #[test]
    fn project_name_new_double_dash_rejected() {
        let err = ProjectName::new("p--x").unwrap_err();
        assert!(
            matches!(err, ProjectNameError::AmbiguousDelimiter(_)),
            "expected AmbiguousDelimiter, got: {err:?}"
        );
    }

    #[test]
    fn project_name_new_leading_dash_rejected() {
        assert!(ProjectName::new("-foo").is_err());
    }

    #[test]
    fn project_name_new_trailing_dash_rejected() {
        assert!(ProjectName::new("foo-").is_err());
    }

    #[test]
    fn project_name_new_empty_rejected() {
        let err = ProjectName::new("").unwrap_err();
        assert!(
            matches!(
                err,
                ProjectNameError::InvalidRef(crate::naming::RefNameError::Empty)
            ),
            "expected InvalidRef(Empty), got: {err:?}"
        );
    }

    #[test]
    fn project_name_new_git_illegal_char_rejected() {
        let err = ProjectName::new("foo bar").unwrap_err();
        assert!(
            matches!(err, ProjectNameError::InvalidRef(_)),
            "expected InvalidRef, got: {err:?}"
        );
    }

    #[test]
    fn project_name_deserialize_rejects_double_dash() {
        let result: Result<ProjectName, _> = from_scalar("p--x");
        assert!(result.is_err(), "double-dash project name must be rejected");
    }

    #[test]
    fn project_name_deserialize_accepts_multi_segment() {
        let name: ProjectName = from_scalar("chatly/web-app").unwrap();
        assert_eq!(name.as_str(), "chatly/web-app");
    }

    /// `ProjectName::new` and the serde `Deserialize` impl run the same
    /// validation, so a project name that reaches rwv through a manifest
    /// is refused just as reliably as one built in-process.
    #[test]
    fn project_name_new_and_serde_agree_on_rejection() {
        assert!(ProjectName::new("p--x").is_err());
        let via_serde: Result<ProjectName, _> = from_scalar("p--x");
        assert!(via_serde.is_err());
    }

    // ========================================================================
    // WorkweaveName::new — parse-boundary validation
    // ========================================================================

    #[test]
    fn workweave_name_new_simple_accepted() {
        assert_eq!(WorkweaveName::new("agent-42").unwrap().as_str(), "agent-42");
    }

    /// The vulnerability this type exists to close: a workweave named with a
    /// `/` mints an ephemeral ref name that [`crate::vcs::LegacyEphemeralRefName::claim`]
    /// would read as a *different* live workweave's pre-flat segmented ref.
    #[test]
    fn workweave_name_new_slash_rejected() {
        let err = WorkweaveName::new("feat-a/main").unwrap_err();
        assert!(
            matches!(err, WorkweaveNameError::Slash(_)),
            "expected Slash, got: {err:?}"
        );
    }

    #[test]
    fn workweave_name_new_double_dash_rejected() {
        let err = WorkweaveName::new("b--c").unwrap_err();
        assert!(
            matches!(err, WorkweaveNameError::AmbiguousDelimiter(_)),
            "expected AmbiguousDelimiter, got: {err:?}"
        );
    }

    #[test]
    fn workweave_name_new_leading_dash_rejected() {
        assert!(WorkweaveName::new("-foo").is_err());
    }

    #[test]
    fn workweave_name_new_trailing_dash_rejected() {
        assert!(WorkweaveName::new("foo-").is_err());
    }

    #[test]
    fn workweave_name_new_empty_rejected() {
        let err = WorkweaveName::new("").unwrap_err();
        assert!(
            matches!(
                err,
                WorkweaveNameError::InvalidRef(crate::naming::RefNameError::Empty)
            ),
            "expected InvalidRef(Empty), got: {err:?}"
        );
    }

    #[test]
    fn workweave_name_deserialize_rejects_slash() {
        let result: Result<WorkweaveName, _> = from_scalar("feat-a/main");
        assert!(
            result.is_err(),
            "slash-containing workweave name must be rejected"
        );
    }

    #[test]
    fn workweave_name_deserialize_rejects_double_dash() {
        let result: Result<WorkweaveName, _> = from_scalar("feat--v2--rc1");
        assert!(
            result.is_err(),
            "double-dash workweave name must be rejected"
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
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains(Manifest::FILE_NAME),
            "error should name the missing file: {msg}"
        );
    }

    #[test]
    fn project_from_dir_manifest_only_no_lock() {
        let dir = tempfile::tempdir().unwrap();
        // A tempdir's own name (e.g. `.tmpXXXXXX`) is not a valid project
        // name (leading `.`), and `Project::from_dir` now enforces that even
        // on the no-`projects/`-ancestor fallback — so tests exercising this
        // loader nest under a plain-named subdirectory instead.
        let project_dir = dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join(Manifest::FILE_NAME), MINIMAL_MANIFEST).unwrap();

        let project = Project::from_dir(&project_dir).unwrap();
        assert!(project.lock.is_none());
        assert_eq!(project.manifest.repositories.len(), 1);
        assert_eq!(project.dir, project_dir);
    }

    /// The loader reads two files, so its error has to say which one broke —
    /// naming the one that parsed cleanly sends the operator to edit a healthy
    /// file.
    #[test]
    fn project_from_dir_bad_lock_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(Manifest::FILE_NAME), MINIMAL_MANIFEST).unwrap();
        std::fs::write(dir.path().join(LockFile::FILE_NAME), "{{bad").unwrap();

        let result = Project::from_dir(dir.path());
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains(LockFile::FILE_NAME),
            "error should name the lock: {msg}"
        );
        assert!(
            !msg.contains(Manifest::FILE_NAME),
            "error must not name the manifest, which parsed: {msg}"
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
        // See project_from_dir_manifest_only_no_lock: nest under a
        // plain-named subdirectory so the derived project name is valid.
        let project_dir = dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join(Manifest::FILE_NAME), MINIMAL_MANIFEST).unwrap();
        let conflict_content = "\
<<<<<<< HEAD\nworkweave: hotfix\nrepositories: {}\n=======\nrepositories: {}\n>>>>>>> abc1234\n";
        std::fs::write(project_dir.join("rwv.lock"), conflict_content).unwrap();

        // Strict loader must fail.
        assert!(
            Project::from_dir(&project_dir).is_err(),
            "from_dir must fail when rwv.lock contains conflict markers"
        );

        // Lockless loader must succeed and return lock: None.
        let project = Project::from_dir_skip_lock(&project_dir).unwrap();
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
        // See project_from_dir_manifest_only_no_lock: nest under a
        // plain-named subdirectory so the derived project name is valid.
        let project_dir = dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join(Manifest::FILE_NAME), MINIMAL_MANIFEST).unwrap();
        // No rwv.lock written.

        let project = Project::from_dir_skip_lock(&project_dir).unwrap();
        assert!(
            project.lock.is_none(),
            "from_dir_skip_lock must return lock: None when rwv.lock is absent"
        );
        assert_eq!(project.manifest.repositories.len(), 1);
    }

    /// rwv.lock exists but is empty (zero bytes).
    /// from_dir fails (an empty file has no `repositories`, so it is not a LockFile);
    /// from_dir_skip_lock succeeds.
    #[test]
    fn project_from_dir_skip_lock_succeeds_with_empty_lock() {
        let dir = tempfile::tempdir().unwrap();
        // See project_from_dir_manifest_only_no_lock: nest under a
        // plain-named subdirectory so the derived project name is valid.
        let project_dir = dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join(Manifest::FILE_NAME), MINIMAL_MANIFEST).unwrap();
        std::fs::write(project_dir.join("rwv.lock"), "").unwrap();

        // Strict loader must fail on an empty file.
        assert!(
            Project::from_dir(&project_dir).is_err(),
            "from_dir must fail when rwv.lock is empty"
        );

        // Lockless loader must succeed.
        let project = Project::from_dir_skip_lock(&project_dir).unwrap();
        assert!(
            project.lock.is_none(),
            "from_dir_skip_lock must return lock: None when rwv.lock is empty"
        );
        assert_eq!(project.manifest.repositories.len(), 1);
    }

    /// No rwv.toml either — from_dir_skip_lock must still fail (manifest is required).
    #[test]
    fn project_from_dir_skip_lock_fails_without_manifest() {
        let dir = tempfile::tempdir().unwrap();
        // Neither rwv.toml nor rwv.lock is present.

        let result = Project::from_dir_skip_lock(dir.path());
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains(Manifest::FILE_NAME),
            "error should name the missing file: {msg}"
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
        std::fs::write(project_dir.join(Manifest::FILE_NAME), MINIMAL_MANIFEST).unwrap();

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
        std::fs::write(project_dir.join(Manifest::FILE_NAME), MINIMAL_MANIFEST).unwrap();

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
        std::fs::write(project_dir.join(Manifest::FILE_NAME), MINIMAL_MANIFEST).unwrap();

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
        let m: Manifest = toml::from_str("[repositories]\n").unwrap();
        assert!(m.repositories.is_empty());
        assert!(m.integrations.is_empty());
    }

    #[test]
    fn lock_empty_repositories() {
        let json = r#"{"repositories": {}}"#;
        let lock: LockFile = serde_json::from_str(json).unwrap();
        assert!(lock.repositories.is_empty());
    }

    // ========================================================================
    // LockConfig serde
    // ========================================================================

    /// Every verb that edits the manifest rewrites the whole file through
    /// [`Manifest::write`], so a policy that does not survive a rewrite is one
    /// `rwv add` away from being revoked without anyone touching it.
    #[test]
    fn lock_policy_survives_a_manifest_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(Manifest::FILE_NAME);
        std::fs::write(&path, "[repositories]\n\n[lock]\nforgo-tag-names = true\n").unwrap();

        let manifest = Manifest::from_path(&path).unwrap();
        assert!(manifest.forgo_tag_names());

        manifest.write(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            Manifest::from_path(&path).unwrap().forgo_tag_names(),
            "the policy must survive a rewrite, got: {text}"
        );
    }

    /// The default writes nothing. A project that never asked for the policy
    /// must not start carrying a `[lock]` table the first time a verb rewrites
    /// its manifest — an unasked-for key reads as a decision someone made.
    #[test]
    fn a_manifest_without_the_policy_writes_no_lock_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(Manifest::FILE_NAME);
        std::fs::write(&path, MINIMAL_MANIFEST).unwrap();

        let manifest = Manifest::from_path(&path).unwrap();
        assert!(!manifest.forgo_tag_names());

        manifest.write(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("[lock]") && !text.contains("forgo-tag-names"),
            "an unset policy must not be written back, got: {text}"
        );
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
        let text = toml::to_string(&original).unwrap();
        let restored: WorkweaveConfig = toml::from_str(&text).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn manifest_with_workweave_section() {
        let text = r#"
[repositories."github/acme/server"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"

[workweave]
link = ["target/"]
copy = [".env"]
"#;
        let m: Manifest = toml::from_str(text).unwrap();
        let ww = m.workweave.expect("workweave should be Some");
        assert_eq!(ww.link, vec!["target/"]);
        assert_eq!(ww.copy, vec![".env"]);
    }

    #[test]
    fn manifest_without_workweave_section() {
        let m: Manifest = toml::from_str(VALID_MANIFEST).unwrap();
        assert!(m.workweave.is_none());
    }

    // ========================================================================
    // Project::from_dir with a lock present
    // ========================================================================

    #[test]
    fn project_from_dir_with_lock() {
        let dir = tempfile::tempdir().unwrap();
        // See project_from_dir_manifest_only_no_lock: nest under a
        // plain-named subdirectory so the derived project name is valid.
        let project_dir = dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join(Manifest::FILE_NAME), VALID_MANIFEST).unwrap();
        std::fs::write(project_dir.join("rwv.lock"), VALID_LOCK).unwrap();

        let project = Project::from_dir(&project_dir).unwrap();
        assert!(project.lock.is_some());
        let lock = project.lock.unwrap();
        assert_eq!(lock.repositories.len(), 1);
    }

    // ========================================================================
    // Manifest accessor methods — iter_repo_paths / get_entry / iter_entries
    // ========================================================================

    // -- iter_repo_paths ------------------------------------------------------

    #[test]
    fn iter_repo_paths_empty_manifest() {
        let m: Manifest = toml::from_str("[repositories]\n").unwrap();
        let paths: Vec<_> = m.iter_repo_paths().collect();
        assert!(paths.is_empty());
    }

    #[test]
    fn iter_repo_paths_single_repo() {
        let m: Manifest = toml::from_str(MINIMAL_MANIFEST).unwrap();
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
        let m: Manifest = toml::from_str(VALID_MANIFEST).unwrap();
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
        let m: Manifest = toml::from_str("[repositories]\n").unwrap();
        let result = m.get_entry(&RepoPath::new("github/acme/server").expect("known-safe literal"));
        assert!(result.is_none());
    }

    #[test]
    fn get_entry_present_returns_some() {
        let m: Manifest = toml::from_str(MINIMAL_MANIFEST).unwrap();
        let entry = m.get_entry(&RepoPath::new("github/acme/server").expect("known-safe literal"));
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.role, Role::Owned);
        assert_eq!(entry.version, RefName::new("main"));
    }

    #[test]
    fn get_entry_absent_path_returns_none() {
        let m: Manifest = toml::from_str(MINIMAL_MANIFEST).unwrap();
        let result =
            m.get_entry(&RepoPath::new("github/acme/nonexistent").expect("known-safe literal"));
        assert!(result.is_none());
    }

    #[test]
    fn get_entry_multi_repo_each_lookup() {
        let m: Manifest = toml::from_str(VALID_MANIFEST).unwrap();
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
        let m: Manifest = toml::from_str("[repositories]\n").unwrap();
        let entries: Vec<_> = m.iter_entries().collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn iter_entries_single_repo() {
        let m: Manifest = toml::from_str(MINIMAL_MANIFEST).unwrap();
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
        let m: Manifest = toml::from_str(VALID_MANIFEST).unwrap();
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
        let m: Manifest = toml::from_str(VALID_MANIFEST).unwrap();
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
        let m: Manifest = toml::from_str("[repositories]\n").unwrap();
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn len_single_repo() {
        let m: Manifest = toml::from_str(MINIMAL_MANIFEST).unwrap();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn len_multi_repo() {
        let m: Manifest = toml::from_str(VALID_MANIFEST).unwrap();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn is_empty_empty_manifest() {
        let m: Manifest = toml::from_str("[repositories]\n").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn is_empty_single_repo() {
        let m: Manifest = toml::from_str(MINIMAL_MANIFEST).unwrap();
        assert!(!m.is_empty());
    }

    #[test]
    fn is_empty_multi_repo() {
        let m: Manifest = toml::from_str(VALID_MANIFEST).unwrap();
        assert!(!m.is_empty());
    }

    // ========================================================================
    // Manifest deny_unknown_fields — top-level key rejection
    // ========================================================================

    /// A misspelled top-level table is rejected with the offending key name,
    /// its line, and the nearest accepted spelling.  The canonical example:
    /// `[lokc]` instead of `[lock]`.
    ///
    /// Rendered `{err:#}` because that is what surfaces this error to an
    /// operator; `{err}` would read only [`Manifest::unparseable_hint`].
    #[test]
    fn misspelled_lock_table_is_refused_with_key_line_and_suggestion() {
        let text = "[repositories]\n\n[lokc]\nforgo-tag-names = true\n";
        let err = Manifest::from_toml_str(text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("lokc"),
            "refusal must name the offending key, got: {msg}"
        );
        assert!(
            msg.contains("line 3"),
            "refusal must locate the offending line, got: {msg}"
        );
        assert!(
            msg.contains("lock"),
            "refusal must suggest the nearest accepted spelling, got: {msg}"
        );
    }

    /// `[integrationz]` is a one-character typo of `integrations` — refuse it
    /// with the nearest spelling.
    #[test]
    fn misspelled_integrations_table_is_refused_with_suggestion() {
        let text = "[repositories]\n\n[integrationz]\nenabled = true\n";
        let err = Manifest::from_toml_str(text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("integrationz"),
            "refusal must name the offending key, got: {msg}"
        );
        assert!(
            msg.contains("integrations"),
            "refusal must suggest the nearest accepted spelling, got: {msg}"
        );
    }

    /// `[workweve]` is a one-character typo of `workweave` — refuse with
    /// the nearest spelling.
    #[test]
    fn misspelled_workweave_table_is_refused_with_suggestion() {
        let text = "[repositories]\n\n[workweve]\nlink = []\n";
        let err = Manifest::from_toml_str(text).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("workweve"),
            "refusal must name the offending key, got: {msg}"
        );
        assert!(
            msg.contains("workweave"),
            "refusal must suggest the nearest accepted spelling, got: {msg}"
        );
    }

    /// A correct manifest parses without error regardless of which top-level
    /// tables are present.
    #[test]
    fn correct_manifest_with_all_sections_parses() {
        let text = r#"
[repositories."github/acme/server"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"

[integrations.cargo]
enabled = true

[workweave]
link = ["target/"]

[lock]
forgo-tag-names = true
"#;
        let m = Manifest::from_toml_str(text).unwrap();
        assert_eq!(m.len(), 1);
        assert!(m.forgo_tag_names());
        assert!(m.workweave.is_some());
        assert_eq!(m.integrations.len(), 1);
    }

    /// Top-level strictness must not leak into `[integrations]`: each
    /// integration's private keys are deliberately open and must keep parsing.
    /// This pins the invariant that `deny_unknown_fields` on `Manifest` applies
    /// only to the top level, not to `IntegrationConfig`'s transparent map.
    #[test]
    fn integration_private_keys_are_unaffected_by_top_level_strictness() {
        let text = r#"
[repositories."github/acme/server"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"

[integrations.cargo-workspace]
enabled = true
patch = "derived"
workspace-package = true
some-private-key = "value"
another-custom-setting = 42
"#;
        let m = Manifest::from_toml_str(text).expect(
            "integration private keys must parse successfully; \
             top-level deny_unknown_fields must not leak into IntegrationConfig",
        );
        assert_eq!(m.integrations.len(), 1);
        let cw = m.integrations.get("cargo-workspace").unwrap();
        assert_eq!(cw.enabled(), Some(true));
    }

    // ========================================================================
    // nearest_manifest_key — unit tests for the edit-distance helper
    // ========================================================================

    #[test]
    fn nearest_key_exact_match_returns_itself() {
        assert_eq!(nearest_manifest_key("lock"), Some("lock"));
        assert_eq!(nearest_manifest_key("repositories"), Some("repositories"));
        assert_eq!(nearest_manifest_key("integrations"), Some("integrations"));
        assert_eq!(nearest_manifest_key("workweave"), Some("workweave"));
    }

    #[test]
    fn nearest_key_transposition_lokc_to_lock() {
        // `lokc` is a transposition of `lock` — OSA counts this as 1 op.
        assert_eq!(nearest_manifest_key("lokc"), Some("lock"));
    }

    #[test]
    fn nearest_key_one_char_typo() {
        assert_eq!(nearest_manifest_key("loxk"), Some("lock"));
        assert_eq!(nearest_manifest_key("integrationz"), Some("integrations"));
        assert_eq!(nearest_manifest_key("workweve"), Some("workweave"));
    }

    #[test]
    fn nearest_key_completely_unrelated_returns_none() {
        // A key with no resemblance to any accepted spelling should not match.
        assert_eq!(nearest_manifest_key("xyzzy"), None);
        assert_eq!(nearest_manifest_key("metadata"), None);
    }
}
