//! Manifest types: `rwv.yaml` and `rwv.lock` parsing and representation.
//!
//! These types model the on-disk YAML format and the resolved in-memory
//! representation. Parsing produces a `Manifest`; locking produces a `LockFile`.

use crate::vcs::{RefName, RevisionId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

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
// Role — change-resistance level for a repo within a project
// ---------------------------------------------------------------------------

/// How freely code in this repo may be modified within the owning project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum Role {
    /// Your code. Change freely.
    Primary,
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
            Role::Primary => "primary",
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
    pub url: String,
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
    /// Deserializes the full mapping into `T`. Returns `T::default()` when
    /// parsing fails or when required fields are absent, so callers get
    /// graceful degradation rather than hard errors.
    pub fn settings<T: serde::de::DeserializeOwned + Default>(&self) -> T {
        serde_yaml::from_value(serde_yaml::Value::Mapping(self.0.clone())).unwrap_or_else(|e| {
            eprintln!(
                "[warning] integration config: failed to parse settings, using defaults: {e}"
            );
            T::default()
        })
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub repositories: BTreeMap<RepoPath, RepoEntry>,
    #[serde(default)]
    pub integrations: BTreeMap<String, IntegrationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workweave: Option<WorkweaveConfig>,
}

impl Manifest {
    /// Load from a YAML file.
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let manifest: Self = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse rwv.yaml at {}: {e}", path.display()))?;
        Ok(manifest)
    }
}

// ---------------------------------------------------------------------------
// Lock file — pinned SHAs
// ---------------------------------------------------------------------------

/// A single entry in an `rwv.lock` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEntry {
    #[serde(rename = "type")]
    pub vcs_type: VcsType,
    pub url: String,
    pub version: RevisionId,
}

/// A parsed `rwv.lock` file — pinned SHAs for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    /// Which workweave this lock was generated from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "weave")]
    pub workweave: Option<WorkweaveName>,
    pub repositories: BTreeMap<RepoPath, LockEntry>,
}

impl LockFile {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let lock: Self = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse rwv.lock at {}: {e}", path.display()))?;
        Ok(lock)
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
    use crate::vcs::{RefName, RevisionId};

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
        let settings: TestSettings = config.settings();
        assert_eq!(settings.files, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn integration_config_settings_returns_default_when_keys_missing() {
        let config = IntegrationConfig::from_yaml("enabled: true");
        let settings: TestSettings = config.settings();
        assert_eq!(settings, TestSettings::default());
    }

    #[test]
    fn integration_config_settings_returns_default_on_wrong_type() {
        // `files` expects a sequence, but we supply a scalar — graceful degradation.
        let config = IntegrationConfig::from_yaml("files: not-a-list");
        let settings: TestSettings = config.settings();
        assert_eq!(settings, TestSettings::default());
    }

    #[test]
    fn integration_config_arbitrary_keys_round_trip() {
        // IntegrationConfig should preserve unknown keys through serde.
        let yaml = "enabled: true\nfiles:\n  - x.json\ncount: 42\n";
        let config: IntegrationConfig = serde_yaml::from_str(yaml).unwrap();
        let restored = serde_yaml::to_string(&config).unwrap();
        let config2: IntegrationConfig = serde_yaml::from_str(&restored).unwrap();
        assert_eq!(config2.enabled(), Some(true));
        let settings: TestSettings = config2.settings();
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
    role: primary
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
    role: primary
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
    fn role_primary_is_active() {
        assert!(Role::Primary.is_active());
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
    role: primary
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
        for role in [Role::Primary, Role::Fork, Role::Dependency, Role::Reference] {
            let yaml = serde_yaml::to_string(&role).unwrap();
            let restored: Role = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(role, restored);
        }
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
    role: primary
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
}
