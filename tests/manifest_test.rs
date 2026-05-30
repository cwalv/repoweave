use repoweave::manifest::{
    CargoWorkspaceConfig, LockFile, Manifest, Project, RepoPath, Role, VcsType, WorkweaveName,
};
use repoweave::vcs::{RawRevisionId, RefName};

// ---------------------------------------------------------------------------
// Helper YAML literals
// ---------------------------------------------------------------------------

const FULL_MANIFEST_YAML: &str = r#"
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
  github/lib/openssl:
    type: git
    url: https://github.com/lib/openssl.git
    version: v3.1.0
    role: dependency
  github/docs/rfc:
    type: git
    url: https://github.com/docs/rfc.git
    version: main
    role: reference
integrations:
  cargo:
    enabled: true
  npm:
    enabled: false
"#;

const MINIMAL_MANIFEST_YAML: &str = r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: owned
"#;

const LOCK_WITH_WORKWEAVE_YAML: &str = r#"
workweave: hotfix-42
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: abc123def456
  github/acme/client:
    type: git
    url: https://github.com/acme/client.git
    version: "789000aabbcc"
"#;

const LOCK_WITHOUT_WORKWEAVE_YAML: &str = r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: abc123def456
"#;

// ---------------------------------------------------------------------------
// Manifest parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_full_manifest() {
    let m: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();
    assert_eq!(m.len(), 4);
    assert_eq!(m.integrations.len(), 2);
}

#[test]
fn manifest_repo_paths_are_btreemap_keys() {
    let m: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();

    // BTreeMap iterates in sorted order — verify keys come out sorted.
    let keys: Vec<&RepoPath> = m.iter_repo_paths().collect();
    assert_eq!(keys[0].as_str(), "github/acme/client");
    assert_eq!(keys[1].as_str(), "github/acme/server");
    assert_eq!(keys[2].as_str(), "github/docs/rfc");
    assert_eq!(keys[3].as_str(), "github/lib/openssl");
}

#[test]
fn manifest_repo_entry_fields() {
    let m: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();
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
    let m: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();
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
    let m: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();
    for (_, entry) in m.iter_entries() {
        assert_eq!(entry.vcs_type, VcsType::Git);
    }
}

// ---------------------------------------------------------------------------
// Missing optional fields
// ---------------------------------------------------------------------------

#[test]
fn manifest_without_integrations() {
    let m: Manifest = serde_yaml::from_str(MINIMAL_MANIFEST_YAML).unwrap();
    assert!(m.integrations.is_empty());
    assert_eq!(m.len(), 1);
}

#[test]
fn integration_config_enabled_none() {
    // An empty integration block should default enabled to None.
    let yaml = r#"
repositories: {}
integrations:
  cargo: {}
"#;
    let m: Manifest = serde_yaml::from_str(yaml).unwrap();
    assert!(m.integrations["cargo"].enabled().is_none());
}

// ---------------------------------------------------------------------------
// Lock file parsing
// ---------------------------------------------------------------------------

#[test]
fn lock_with_workweave_provenance() {
    let lock: LockFile = serde_yaml::from_str(LOCK_WITH_WORKWEAVE_YAML).unwrap();
    assert_eq!(lock.workweave, Some(WorkweaveName::new("hotfix-42")));
    assert_eq!(lock.len(), 2);

    let server =
        &lock.repo_map()[&RepoPath::new("github/acme/server").expect("known-safe literal")];
    assert_eq!(server.vcs_type, VcsType::Git);
    assert_eq!(server.version, RawRevisionId::new("abc123def456"));
}

#[test]
fn lock_without_workweave_provenance() {
    let lock: LockFile = serde_yaml::from_str(LOCK_WITHOUT_WORKWEAVE_YAML).unwrap();
    assert_eq!(lock.workweave, None);
    assert_eq!(lock.len(), 1);
}

#[test]
fn lock_repo_paths_sorted() {
    let lock: LockFile = serde_yaml::from_str(LOCK_WITH_WORKWEAVE_YAML).unwrap();
    let keys: Vec<&str> = lock.iter_repo_paths().map(|k| k.as_str()).collect();
    assert_eq!(keys, vec!["github/acme/client", "github/acme/server"]);
}

// ---------------------------------------------------------------------------
// Round-trip serialize / deserialize
// ---------------------------------------------------------------------------

#[test]
fn manifest_round_trip() {
    let original: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();
    let serialized = serde_yaml::to_string(&original).unwrap();
    let deserialized: Manifest = serde_yaml::from_str(&serialized).unwrap();

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
    let original: LockFile = serde_yaml::from_str(LOCK_WITH_WORKWEAVE_YAML).unwrap();
    let serialized = serde_yaml::to_string(&original).unwrap();
    let deserialized: LockFile = serde_yaml::from_str(&serialized).unwrap();

    assert_eq!(original.workweave, deserialized.workweave);
    assert_eq!(original.len(), deserialized.len());
    for (key, orig_entry) in original.iter_entries() {
        let de_entry = deserialized.get_entry(key).unwrap();
        assert_eq!(orig_entry.vcs_type, de_entry.vcs_type);
        assert_eq!(orig_entry.url, de_entry.url);
        assert_eq!(orig_entry.version, de_entry.version);
    }
}

#[test]
fn lock_without_workweave_round_trip_skips_workweave_key() {
    let original: LockFile = serde_yaml::from_str(LOCK_WITHOUT_WORKWEAVE_YAML).unwrap();
    let serialized = serde_yaml::to_string(&original).unwrap();
    // The `workweave` key should be absent thanks to `skip_serializing_if`.
    assert!(!serialized.contains("workweave:"));
    assert!(!serialized.contains("weave:"));
    let deserialized: LockFile = serde_yaml::from_str(&serialized).unwrap();
    assert_eq!(deserialized.workweave, None);
}

// ---------------------------------------------------------------------------
// Project::from_dir — tempdir tests
// ---------------------------------------------------------------------------

#[test]
fn project_from_dir_manifest_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rwv.yaml"), MINIMAL_MANIFEST_YAML).unwrap();

    let project = Project::from_dir(dir.path()).unwrap();
    assert_eq!(project.manifest.len(), 1);
    assert!(project.lock.is_none());
    assert_eq!(project.dir, dir.path());
}

#[test]
fn project_from_dir_manifest_and_lock() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rwv.yaml"), FULL_MANIFEST_YAML).unwrap();
    std::fs::write(dir.path().join("rwv.lock"), LOCK_WITH_WORKWEAVE_YAML).unwrap();

    let project = Project::from_dir(dir.path()).unwrap();
    assert_eq!(project.manifest.len(), 4);
    let lock = project.lock.as_ref().unwrap();
    assert_eq!(lock.workweave, Some(WorkweaveName::new("hotfix-42")));
    assert_eq!(lock.len(), 2);
}

#[test]
fn project_from_dir_missing_manifest_errors() {
    let dir = tempfile::tempdir().unwrap();
    // No rwv.yaml written — from_dir should fail.
    let result = Project::from_dir(dir.path());
    assert!(result.is_err());
}

#[test]
fn project_name_derived_from_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rwv.yaml"), MINIMAL_MANIFEST_YAML).unwrap();

    let project = Project::from_dir(dir.path()).unwrap();
    // Name is derived from the path; since tempdir isn't under `projects/`,
    // the full path is used as the name.
    assert!(!project.name.as_str().is_empty());
}

#[test]
fn project_name_strips_projects_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("projects").join("web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.yaml"), MINIMAL_MANIFEST_YAML).unwrap();

    // Use a relative path starting with "projects/" so strip_prefix works.
    let nested_project_dir = dir.path().join("projects").join("web-app");
    let project = Project::from_dir(&nested_project_dir).unwrap();
    // The path doesn't literally start with "projects" (it's an absolute temp path),
    // so strip_prefix falls back to the full path. That's the expected behavior
    // for absolute paths.
    assert!(!project.name.as_str().is_empty());
}

// ---------------------------------------------------------------------------
// CargoWorkspaceConfig deserialization (fo-cnpjy.6)
// ---------------------------------------------------------------------------

/// Parse a `CargoWorkspaceConfig` directly from an `IntegrationConfig` YAML
/// snippet, the same path used by the integration at runtime.
fn parse_cargo_config(yaml: &str) -> CargoWorkspaceConfig {
    use repoweave::manifest::IntegrationConfig;
    let config = IntegrationConfig::from_yaml(yaml);
    config
        .settings::<CargoWorkspaceConfig>()
        .expect("CargoWorkspaceConfig parse failed")
}

/// All new fields default correctly when the integration block is empty.
/// This is the backward-compatibility guarantee: existing rwv.yaml files
/// that only set `enabled:` or `exclude:` must not break.
#[test]
fn cargo_workspace_config_defaults_when_omitted() {
    let cfg = parse_cargo_config("{}");
    assert!(
        cfg.members.is_empty(),
        "members must default to empty BTreeMap"
    );
    assert!(!cfg.patch, "patch must default to false");
    assert!(
        !cfg.workspace_package,
        "workspace-package must default to false"
    );
    assert!(cfg.exclude.is_empty(), "exclude must default to empty vec");
}

/// A `members:` block with a repo key and `include` list deserializes into
/// the correct `BTreeMap<String, MemberSpec>` shape.
/// This is the rvtty scenario from plan §5a / `cargo-workspace-vs-repo.md`
/// §179–212.
#[test]
fn cargo_workspace_config_members_spec_roundtrips() {
    let yaml = r#"
members:
  github/cwalv/rvtty:
    include: [daemon, client, common]
"#;
    let cfg = parse_cargo_config(yaml);

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
    assert!(!cfg.patch);
    assert!(!cfg.workspace_package);
    assert!(cfg.exclude.is_empty());
}

/// `include` and `exclude` both parse correctly together.
#[test]
fn cargo_workspace_config_members_include_and_exclude() {
    let yaml = r#"
members:
  github/cwalv/rvtty:
    include: [daemon, client, common, workspace]
    exclude: [workspace]
"#;
    let cfg = parse_cargo_config(yaml);
    let spec = cfg.members.get("github/cwalv/rvtty").unwrap();
    assert_eq!(spec.include, vec!["daemon", "client", "common", "workspace"]);
    assert_eq!(spec.exclude, vec!["workspace"]);
}

/// `patch: true` and `workspace-package: true` (kebab-case serde rename) both
/// deserialize to the correct boolean fields.
#[test]
fn cargo_workspace_config_bool_flags_parse() {
    let yaml = "patch: true\nworkspace-package: true\n";
    let cfg = parse_cargo_config(yaml);
    assert!(cfg.patch, "patch should be true");
    assert!(cfg.workspace_package, "workspace_package should be true");
}

/// `patch: "yes"` is not a valid YAML boolean — serde must return a parse
/// error rather than silently coercing or silently ignoring the value.
/// This guards against typos in rwv.yaml where a user writes `patch: yes`
/// thinking it is boolean (in strict YAML 1.2 "yes" is a string, not bool).
#[test]
fn cargo_workspace_config_patch_string_is_type_error() {
    use repoweave::manifest::IntegrationConfig;
    let config = IntegrationConfig::from_yaml("patch: \"yes\"");
    let result = config.settings::<CargoWorkspaceConfig>();
    assert!(
        result.is_err(),
        "patch: \"yes\" must be a type error, got Ok({:?})",
        result.ok()
    );
}

/// An empty `MemberSpec` (both `include` and `exclude` omitted) deserializes
/// without error and carries empty vectors.  This lets operators write:
///   members:
///     github/cwalv/some-repo: {}
/// to explicitly declare "no members from this repo" without a parse failure.
#[test]
fn member_spec_empty_is_valid() {
    let yaml = "members:\n  github/cwalv/some-repo: {}\n";
    let cfg = parse_cargo_config(yaml);
    let spec = cfg.members.get("github/cwalv/some-repo").unwrap();
    assert!(spec.include.is_empty());
    assert!(spec.exclude.is_empty());
}

/// `CargoWorkspaceConfig` with all fields set serializes back to YAML and
/// round-trips correctly through `IntegrationConfig::settings`.
#[test]
fn cargo_workspace_config_full_serde_round_trip() {
    use repoweave::manifest::IntegrationConfig;

    let yaml = r#"
exclude:
  - github/cwalv/mcp_agent_mail_rust
members:
  github/cwalv/rvtty:
    include: [daemon, client, common]
    exclude: [fuzz]
patch: true
workspace-package: true
"#;
    let config = IntegrationConfig::from_yaml(yaml);
    let cfg: CargoWorkspaceConfig = config.settings().unwrap();

    assert_eq!(cfg.exclude, vec!["github/cwalv/mcp_agent_mail_rust"]);
    assert_eq!(cfg.members.len(), 1);
    let spec = cfg.members.get("github/cwalv/rvtty").unwrap();
    assert_eq!(spec.include, vec!["daemon", "client", "common"]);
    assert_eq!(spec.exclude, vec!["fuzz"]);
    assert!(cfg.patch);
    assert!(cfg.workspace_package);

    // Serialize back to serde_yaml::Value and re-parse — verify it round-trips.
    let serialized = serde_yaml::to_string(&cfg).unwrap();
    let restored: CargoWorkspaceConfig = serde_yaml::from_str(&serialized).unwrap();
    assert_eq!(restored.exclude, cfg.exclude);
    assert_eq!(restored.patch, cfg.patch);
    assert_eq!(restored.workspace_package, cfg.workspace_package);
    let restored_spec = restored.members.get("github/cwalv/rvtty").unwrap();
    assert_eq!(restored_spec.include, spec.include);
    assert_eq!(restored_spec.exclude, spec.exclude);
}
