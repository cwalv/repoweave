//! Manifest types: `rwv.yaml` and `rwv.lock` parsing and representation.
//!
//! These types model the on-disk YAML format and the resolved in-memory
//! representation. Parsing produces a `Manifest`; locking produces a `LockFile`.

use crate::registry::RegistryName;
use crate::vcs::{RawRevisionId, RefName, ResolvedRevisionId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Newtypes — distinguish semantically different strings at the type level
// ---------------------------------------------------------------------------

/// A local path relative to the workspace root (e.g., `github/chatly/server`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoPath(String);

impl RepoPath {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
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
/// The `repositories` field is currently `pub` for backwards-compat while
/// call-site migrations (fo-lokti.2, fo-lokti.3) are in progress. Once those
/// siblings close, the field will be narrowed to `pub(crate)` or private.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    // `pub` temporarily while call-sites migrate to accessor methods
    // (fo-lokti.2, fo-lokti.3). Will become `pub(crate)` once both close.
    pub repositories: BTreeMap<RepoPath, RepoEntry>,
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
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        Self::from_yaml_str(&content).map_err(|e| {
            anyhow::anyhow!("failed to parse rwv.yaml at {}: {e}", path.display())
        })
    }

    /// Parse a manifest from a YAML string, surfacing the
    /// legacy-`role: primary` migration hint when the parser rejects an
    /// otherwise-recognisable manifest.
    ///
    /// fo-fzf4n drops the back-compat alias on `role: primary`; manifests
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
                    Err(anyhow::anyhow!("{err}"))
                }
            }
        }
    }
}

/// True iff `content` contains at least one `role: primary` line where
/// `primary` is the *full* value (not a prefix like `primary_repo`).
///
/// Used by the manifest loader and `rwv doctor` to detect the legacy
/// spelling that lost its serde alias in fo-fzf4n. Targeted regex over
/// raw text avoids a full YAML round-trip, which would destroy comments
/// and key ordering when later rewriting the file under `--fix`.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    /// Which workweave this lock was generated from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "weave")]
    pub workweave: Option<WorkweaveName>,
    pub repositories: BTreeMap<RepoPath, LockEntry>,
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
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedLockFile {
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "weave")]
    pub workweave: Option<WorkweaveName>,
    pub repositories: BTreeMap<RepoPath, ResolvedLockEntry>,
}

impl LockFile {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let lock: Self = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse rwv.lock at {}: {e}", path.display()))?;
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
    /// Load a project from its directory.
    pub fn from_dir(dir: &Path) -> anyhow::Result<Self> {
        let manifest_path = dir.join("rwv.yaml");
        let manifest = Manifest::from_path(&manifest_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to load manifest at {}: {}",
                manifest_path.display(),
                e
            )
        })?;
        let lock_path = dir.join("rwv.lock");
        let lock = if lock_path.exists() {
            Some(LockFile::from_path(&lock_path).map_err(|e| {
                anyhow::anyhow!("failed to load lock at {}: {}", lock_path.display(), e)
            })?)
        } else {
            None
        };

        // Derive project name from directory structure.
        // `projects/web-app/` → "web-app"
        // `projects/chatly/web-app/` → "chatly/web-app"
        let name = dir
            .strip_prefix("projects")
            .unwrap_or(dir)
            .to_string_lossy()
            .into_owned();

        Ok(Self {
            dir: dir.to_path_buf(),
            name: ProjectName::new(name),
            manifest,
            lock,
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
            msg.contains("No such file") || msg.contains("not found") || msg.contains("os error"),
            "expected IO error, got: {msg}"
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

    /// fo-fzf4n removed the back-compat alias on `role: primary`. A bare
    /// `primary` scalar must no longer deserialize as `Role::Owned` —
    /// otherwise the doctor-fix migration path wouldn't trigger.
    #[test]
    fn role_primary_yaml_no_longer_deserializes() {
        assert!(
            serde_yaml::from_str::<Role>("primary").is_err(),
            "after fo-fzf4n, `primary` must not parse as Role"
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
        let rp = RepoPath::new("github/acme/server");
        assert_eq!(rp.as_path(), Path::new("github/acme/server"));
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

    #[test]
    fn project_name_from_projects_relative_path() {
        // When dir is a relative path starting with "projects/", the prefix is stripped.
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("projects").join("my-app");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("rwv.yaml"), MINIMAL_MANIFEST).unwrap();

        // Use a relative path so strip_prefix("projects") works.
        let relative = PathBuf::from("projects/my-app");
        // We can't use from_dir with the relative path because the file won't be found.
        // Instead, verify the name derivation logic directly.
        let name = relative
            .strip_prefix("projects")
            .unwrap_or(&relative)
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, "my-app");
    }

    #[test]
    fn project_name_nested_under_projects() {
        let relative = PathBuf::from("projects/chatly/web-app");
        let name = relative
            .strip_prefix("projects")
            .unwrap_or(&relative)
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, "chatly/web-app");
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
        assert_eq!(paths[0], &RepoPath::new("github/acme/server"));
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
        let result = m.get_entry(&RepoPath::new("github/acme/server"));
        assert!(result.is_none());
    }

    #[test]
    fn get_entry_present_returns_some() {
        let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST).unwrap();
        let entry = m.get_entry(&RepoPath::new("github/acme/server"));
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.role, Role::Owned);
        assert_eq!(entry.version, RefName::new("main"));
    }

    #[test]
    fn get_entry_absent_path_returns_none() {
        let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST).unwrap();
        let result = m.get_entry(&RepoPath::new("github/acme/nonexistent"));
        assert!(result.is_none());
    }

    #[test]
    fn get_entry_multi_repo_each_lookup() {
        let m: Manifest = serde_yaml::from_str(VALID_MANIFEST).unwrap();
        let server = m.get_entry(&RepoPath::new("github/acme/server"));
        let client = m.get_entry(&RepoPath::new("github/acme/client"));
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
        assert_eq!(path, &RepoPath::new("github/acme/server"));
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
            let looked_up = m.get_entry(path).expect("get_entry must find iter_entries path");
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
