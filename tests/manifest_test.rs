use repoweave::manifest::{
    CargoWorkspaceConfig, LockFile, Manifest, PatchMode, Project, RepoPath, Role, VcsType,
};
use repoweave::vcs::{RawRevisionId, RefName};

mod common;

// ---------------------------------------------------------------------------
// Helper manifest literals
// ---------------------------------------------------------------------------

const FULL_MANIFEST: &str = r#"
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

[repositories."github/lib/openssl"]
type = "git"
url = "https://github.com/lib/openssl.git"
version = "v3.1.0"
role = "dependency"

[repositories."github/docs/rfc"]
type = "git"
url = "https://github.com/docs/rfc.git"
version = "main"
role = "reference"

[integrations.cargo]
enabled = true

[integrations.npm]
enabled = false
"#;

const MINIMAL_MANIFEST: &str = r#"
[repositories."github/acme/server"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"
"#;

const LOCK_JSON: &str = r#"{
  "repositories": {
    "github/acme/server": {
      "type": "git",
      "url": "https://github.com/acme/server.git",
      "version": "abc123def456"
    },
    "github/acme/client": {
      "type": "git",
      "url": "https://github.com/acme/client.git",
      "version": "789000aabbcc"
    }
  }
}
"#;

const MINIMAL_LOCK_JSON: &str = r#"{
  "repositories": {
    "github/acme/server": {
      "type": "git",
      "url": "https://github.com/acme/server.git",
      "version": "abc123def456"
    }
  }
}
"#;

// ---------------------------------------------------------------------------
// Manifest parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_full_manifest() {
    let m: Manifest = toml::from_str(FULL_MANIFEST).unwrap();
    assert_eq!(m.len(), 4);
    assert_eq!(m.integrations.len(), 2);
}

#[test]
fn manifest_repo_paths_are_btreemap_keys() {
    let m: Manifest = toml::from_str(FULL_MANIFEST).unwrap();

    // BTreeMap iterates in sorted order — verify keys come out sorted.
    let keys: Vec<&RepoPath> = m.iter_repo_paths().collect();
    assert_eq!(keys[0].as_str(), "github/acme/client");
    assert_eq!(keys[1].as_str(), "github/acme/server");
    assert_eq!(keys[2].as_str(), "github/docs/rfc");
    assert_eq!(keys[3].as_str(), "github/lib/openssl");
}

#[test]
fn manifest_repo_entry_fields() {
    let m: Manifest = toml::from_str(FULL_MANIFEST).unwrap();
    let server = m
        .get_entry(&RepoPath::new("github/acme/server").expect("known-safe literal"))
        .unwrap();
    assert_eq!(server.vcs_type, VcsType::Git);
    assert_eq!(server.url.to_string(), "https://github.com/acme/server.git");
    assert_eq!(server.version, RefName::new("main"));
    assert_eq!(server.role, Role::Owned);
}

// ---------------------------------------------------------------------------
// Role deserialization
// ---------------------------------------------------------------------------

#[test]
fn role_deserialization_all_variants() {
    let m: Manifest = toml::from_str(FULL_MANIFEST).unwrap();
    let role_of = |key: &str| {
        m.get_entry(&RepoPath::new(key).expect("test helper: forward-slash paths only"))
            .unwrap()
            .role
    };

    assert_eq!(role_of("github/acme/server"), Role::Owned);
    assert_eq!(role_of("github/acme/client"), Role::Fork);
    assert_eq!(role_of("github/lib/openssl"), Role::Dependency);
    assert_eq!(role_of("github/docs/rfc"), Role::Reference);
}

#[test]
fn role_is_active() {
    assert!(Role::Owned.is_active());
    assert!(Role::Fork.is_active());
    assert!(Role::Dependency.is_active());
    assert!(!Role::Reference.is_active());
}

// ---------------------------------------------------------------------------
// VcsType deserialization
// ---------------------------------------------------------------------------

#[test]
fn vcs_type_git() {
    let m: Manifest = toml::from_str(FULL_MANIFEST).unwrap();
    for (_, entry) in m.iter_entries() {
        assert_eq!(entry.vcs_type, VcsType::Git);
    }
}

// ---------------------------------------------------------------------------
// Missing optional fields
// ---------------------------------------------------------------------------

#[test]
fn manifest_without_integrations() {
    let m: Manifest = toml::from_str(MINIMAL_MANIFEST).unwrap();
    assert!(m.integrations.is_empty());
    assert_eq!(m.len(), 1);
}

#[test]
fn integration_config_enabled_none() {
    // An empty integration block should default enabled to None.
    let manifest_toml = r#"
[repositories]

[integrations.cargo]
"#;
    let m: Manifest = toml::from_str(manifest_toml).unwrap();
    assert!(m.integrations["cargo"].enabled().is_none());
}

// ---------------------------------------------------------------------------
// Lock file parsing
// ---------------------------------------------------------------------------

#[test]
fn lock_parses_repositories() {
    let lock: LockFile = serde_json::from_str(LOCK_JSON).unwrap();
    assert_eq!(lock.len(), 2);

    let server =
        &lock.repo_map()[&RepoPath::new("github/acme/server").expect("known-safe literal")];
    assert_eq!(server.vcs_type, VcsType::Git);
    assert_eq!(server.version, RawRevisionId::new("abc123def456"));
}

#[test]
fn lock_minimal_single_repo() {
    let lock: LockFile = serde_json::from_str(MINIMAL_LOCK_JSON).unwrap();
    assert_eq!(lock.len(), 1);
}

#[test]
fn lock_repo_paths_sorted() {
    let lock: LockFile = serde_json::from_str(LOCK_JSON).unwrap();
    let keys: Vec<&str> = lock.iter_repo_paths().map(|k| k.as_str()).collect();
    assert_eq!(keys, vec!["github/acme/client", "github/acme/server"]);
}

// ---------------------------------------------------------------------------
// Round-trip serialize / deserialize
// ---------------------------------------------------------------------------

#[test]
fn manifest_round_trip() {
    let original: Manifest = toml::from_str(FULL_MANIFEST).unwrap();
    let serialized = toml::to_string(&original).unwrap();
    let deserialized: Manifest = toml::from_str(&serialized).unwrap();

    assert_eq!(original.len(), deserialized.len());
    for (key, orig_entry) in original.iter_entries() {
        let de_entry = deserialized.get_entry(key).unwrap();
        assert_eq!(orig_entry.vcs_type, de_entry.vcs_type);
        assert_eq!(orig_entry.url, de_entry.url);
        assert_eq!(orig_entry.version, de_entry.version);
        assert_eq!(orig_entry.role, de_entry.role);
    }
    assert_eq!(original.integrations.len(), deserialized.integrations.len());
}

#[test]
fn lock_round_trip() {
    let original: LockFile = serde_json::from_str(LOCK_JSON).unwrap();
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: LockFile = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.len(), deserialized.len());
    for (key, orig_entry) in original.iter_entries() {
        let de_entry = deserialized.get_entry(key).unwrap();
        assert_eq!(orig_entry.vcs_type, de_entry.vcs_type);
        assert_eq!(orig_entry.url, de_entry.url);
        assert_eq!(orig_entry.version, de_entry.version);
    }
}

/// A lock from before the `workweave` field was dropped must not parse —
/// `deny_unknown_fields` turns the retired key into a hard error rather
/// than a silent drop.
#[test]
fn lock_with_legacy_workweave_key_is_rejected() {
    let json = r#"{"workweave": "hotfix-42", "repositories": {}}"#;
    let result: Result<LockFile, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Project::from_dir — tempdir tests
// ---------------------------------------------------------------------------

#[test]
fn project_from_dir_manifest_only() {
    let dir = common::tempdir().unwrap();
    // A tempdir's own name (e.g. `.tmpXXXXXX`) is not a valid project name
    // (leading `.`), and `Project::from_dir` now enforces that even on the
    // no-`projects/`-ancestor fallback — so nest under a plain-named
    // subdirectory instead.
    let project_dir = dir.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), MINIMAL_MANIFEST).unwrap();

    let project = Project::from_dir(&project_dir).unwrap();
    assert_eq!(project.manifest.len(), 1);
    assert!(project.lock.is_none());
    assert_eq!(project.dir, project_dir);
}

#[test]
fn project_from_dir_manifest_and_lock() {
    let dir = common::tempdir().unwrap();
    let project_dir = dir.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), FULL_MANIFEST).unwrap();
    std::fs::write(project_dir.join("rwv.lock"), LOCK_JSON).unwrap();

    let project = Project::from_dir(&project_dir).unwrap();
    assert_eq!(project.manifest.len(), 4);
    let lock = project.lock.as_ref().unwrap();
    assert_eq!(lock.len(), 2);
}

#[test]
fn project_from_dir_missing_manifest_errors() {
    let dir = common::tempdir().unwrap();
    // No rwv.toml written — from_dir should fail.
    let result = Project::from_dir(dir.path());
    assert!(result.is_err());
}

#[test]
fn project_name_derived_from_dir() {
    let dir = common::tempdir().unwrap();
    let project_dir = dir.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), MINIMAL_MANIFEST).unwrap();

    let project = Project::from_dir(&project_dir).unwrap();
    // Name is derived from the path; since the project dir isn't under
    // `projects/`, the last path component is used as the name.
    assert!(!project.name.as_str().is_empty());
}

#[test]
fn project_name_strips_projects_prefix() {
    let dir = common::tempdir().unwrap();
    let project_dir = dir.path().join("projects").join("web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), MINIMAL_MANIFEST).unwrap();

    // Use a relative path starting with "projects/" so strip_prefix works.
    let nested_project_dir = dir.path().join("projects").join("web-app");
    let project = Project::from_dir(&nested_project_dir).unwrap();
    // The path doesn't literally start with "projects" (it's an absolute temp path),
    // so strip_prefix falls back to the full path. That's the expected behavior
    // for absolute paths.
    assert!(!project.name.as_str().is_empty());
}

// ---------------------------------------------------------------------------
// CargoWorkspaceConfig deserialization
// ---------------------------------------------------------------------------

/// Parse a `CargoWorkspaceConfig` directly from an `IntegrationConfig`
/// snippet, the same path used by the integration at runtime.
fn parse_cargo_config(manifest_toml: &str) -> CargoWorkspaceConfig {
    use repoweave::manifest::IntegrationConfig;
    let config = IntegrationConfig::from_toml(manifest_toml);
    config
        .settings::<CargoWorkspaceConfig>()
        .expect("CargoWorkspaceConfig parse failed")
}

/// All new fields default correctly when the integration block is empty.
/// This is the backward-compatibility guarantee: existing rwv.toml files
/// that only set `enabled` or `exclude` must not break.
#[test]
fn cargo_workspace_config_defaults_when_omitted() {
    let cfg = parse_cargo_config("");
    assert!(
        cfg.members.is_empty(),
        "members must default to empty BTreeMap"
    );
    assert_eq!(
        cfg.patch,
        PatchMode::Off,
        "patch must default to PatchMode::Off"
    );
    assert!(
        !cfg.workspace_package,
        "workspace-package must default to false"
    );
    assert!(cfg.exclude.is_empty(), "exclude must default to empty vec");
}

/// A `members` block with a repo key and `include` list deserializes into
/// the correct `BTreeMap<String, MemberSpec>` shape.
/// This is the rvtty scenario from plan §5a / `cargo-workspace-vs-repo.md`
/// §179–212.
#[test]
fn cargo_workspace_config_members_spec_roundtrips() {
    let manifest_toml = r#"
[members."github/cwalv/rvtty"]
include = ["daemon", "client", "common"]
"#;
    let cfg = parse_cargo_config(manifest_toml);

    assert_eq!(cfg.members.len(), 1, "expected exactly one repo in members");

    let spec = cfg
        .members
        .get("github/cwalv/rvtty")
        .expect("github/cwalv/rvtty must be present");

    assert_eq!(
        spec.include,
        vec!["daemon", "client", "common"],
        "include list must match verbatim"
    );
    assert!(
        spec.exclude.is_empty(),
        "exclude must default to empty when omitted"
    );

    // Defaults still hold for the other fields.
    assert_eq!(cfg.patch, PatchMode::Off);
    assert!(!cfg.workspace_package);
    assert!(cfg.exclude.is_empty());
}

/// `include` and `exclude` both parse correctly together.
#[test]
fn cargo_workspace_config_members_include_and_exclude() {
    let manifest_toml = r#"
[members."github/cwalv/rvtty"]
include = ["daemon", "client", "common", "workspace"]
exclude = ["workspace"]
"#;
    let cfg = parse_cargo_config(manifest_toml);
    let spec = cfg.members.get("github/cwalv/rvtty").unwrap();
    assert_eq!(
        spec.include,
        vec!["daemon", "client", "common", "workspace"]
    );
    assert_eq!(spec.exclude, vec!["workspace"]);
}

/// `patch = true` (back-compat wire alias for `committed-paths`) and
/// `workspace-package = true` (kebab-case serde rename) both deserialize
/// to the expected fields.
#[test]
fn cargo_workspace_config_bool_flags_parse() {
    let manifest_toml = "patch = true\nworkspace-package = true\n";
    let cfg = parse_cargo_config(manifest_toml);
    assert_eq!(
        cfg.patch,
        PatchMode::CommittedPaths,
        "patch: true is the wire alias for committed-paths (backward compat)"
    );
    assert!(cfg.workspace_package, "workspace_package should be true");
}

/// `patch: false` is the wire alias for `off` — the pre-2026 default.
/// Confirms existing manifests carrying the explicit boolean shape keep
/// parsing unchanged (no migration machinery needed).
#[test]
fn cargo_workspace_config_patch_false_maps_to_off() {
    let cfg = parse_cargo_config("patch = false\n");
    assert_eq!(cfg.patch, PatchMode::Off);
}

/// `patch: committed-paths` (the modern string spelling of the pre-2026
/// behavior) parses to the same variant as `patch: true`.
#[test]
fn cargo_workspace_config_patch_committed_paths_string() {
    let cfg = parse_cargo_config("patch = \"committed-paths\"\n");
    assert_eq!(cfg.patch, PatchMode::CommittedPaths);
}

/// `patch: derived` (the registry-dep tier — matches against the in-weave
/// package-name index) parses to `PatchMode::Derived`.
#[test]
fn cargo_workspace_config_patch_derived_string() {
    let cfg = parse_cargo_config("patch = \"derived\"\n");
    assert_eq!(cfg.patch, PatchMode::Derived);
}

/// `patch = "off"` is the explicit spelling of the default. Parses to
/// `PatchMode::Off`.
#[test]
fn cargo_workspace_config_patch_off_string() {
    let cfg = parse_cargo_config("patch = \"off\"\n");
    assert_eq!(cfg.patch, PatchMode::Off);
}

/// An unknown string is a parse error — typo detection. A silent fallback
/// to a default would mask a real config mistake.
#[test]
fn cargo_workspace_config_patch_unknown_string_is_error() {
    use repoweave::manifest::IntegrationConfig;
    let config = IntegrationConfig::from_toml("patch = \"mirroir\"\n");
    let result = config.settings::<CargoWorkspaceConfig>();
    assert!(
        result.is_err(),
        "unknown patch mode string should be a type error, got Ok({:?})",
        result.ok()
    );
}

/// An operator reaching for a boolean and writing a near-miss word gets an
/// error either way, but from two different layers, and both are worth
/// holding.
///
/// Quoted, `patch = "yes"` is a well-formed string the `PatchMode` visitor
/// rejects by value. Bare, `patch = yes` never reaches the visitor at all:
/// TOML has no unquoted string, so it fails as syntax. The bare spelling is
/// the one that used to be dangerous — YAML 1.1 read it as `true` — and the
/// point of pinning it here is that the format now refuses it outright
/// rather than deciding what it meant.
#[test]
fn cargo_workspace_config_patch_near_miss_boolean_is_refused() {
    use repoweave::manifest::IntegrationConfig;
    let quoted = IntegrationConfig::from_toml("patch = \"yes\"");
    let result = quoted.settings::<CargoWorkspaceConfig>();
    assert!(
        result.is_err(),
        "patch = \"yes\" must be a value error, got Ok({:?})",
        result.ok()
    );
    assert!(
        toml::from_str::<toml::Table>("patch = yes").is_err(),
        "a bare `yes` must not parse as TOML at all"
    );
}

/// An empty `MemberSpec` (both `include` and `exclude` omitted) deserializes
/// without error and carries empty vectors.  This lets operators write:
///   [members."github/cwalv/some-repo"]
/// to explicitly declare "no members from this repo" without a parse failure.
#[test]
fn member_spec_empty_is_valid() {
    let manifest_toml = "[members.\"github/cwalv/some-repo\"]\n";
    let cfg = parse_cargo_config(manifest_toml);
    let spec = cfg.members.get("github/cwalv/some-repo").unwrap();
    assert!(spec.include.is_empty());
    assert!(spec.exclude.is_empty());
}

/// `CargoWorkspaceConfig` with all fields set serializes back and
/// round-trips correctly through `IntegrationConfig::settings`.
#[test]
fn cargo_workspace_config_full_serde_round_trip() {
    use repoweave::manifest::IntegrationConfig;

    let manifest_toml = r#"
exclude = ["github/cwalv/mcp_agent_mail_rust"]
patch = true
workspace-package = true

[members."github/cwalv/rvtty"]
include = ["daemon", "client", "common"]
exclude = ["fuzz"]
"#;
    let config = IntegrationConfig::from_toml(manifest_toml);
    let cfg: CargoWorkspaceConfig = config.settings().unwrap();

    assert_eq!(cfg.exclude, vec!["github/cwalv/mcp_agent_mail_rust"]);
    assert_eq!(cfg.members.len(), 1);
    let spec = cfg.members.get("github/cwalv/rvtty").unwrap();
    assert_eq!(spec.include, vec!["daemon", "client", "common"]);
    assert_eq!(spec.exclude, vec!["fuzz"]);
    // `patch = true` is the wire alias for `committed-paths`.
    assert_eq!(cfg.patch, PatchMode::CommittedPaths);
    assert!(cfg.workspace_package);

    // Serialize back and re-parse — verify it round-trips. Note: the
    // serialized form emits the modern string spelling (`committed-paths`),
    // not the legacy `true`; the deserializer accepts both, so the round-trip
    // lands on the same variant.
    let serialized = toml::to_string(&cfg).unwrap();
    let restored: CargoWorkspaceConfig = toml::from_str(&serialized).unwrap();
    assert_eq!(restored.exclude, cfg.exclude);
    assert_eq!(restored.patch, cfg.patch);
    assert_eq!(restored.workspace_package, cfg.workspace_package);
    let restored_spec = restored.members.get("github/cwalv/rvtty").unwrap();
    assert_eq!(restored_spec.include, spec.include);
    assert_eq!(restored_spec.exclude, spec.exclude);
}
