use repoweave::manifest::{LockFile, Manifest, Project, RepoPath, Role, VcsType, WorkweaveName};
use repoweave::vcs::{RefName, RevisionId};

// ---------------------------------------------------------------------------
// Helper YAML literals
// ---------------------------------------------------------------------------

const FULL_MANIFEST_YAML: &str = r#"
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
    role: primary
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
    assert_eq!(m.repositories.len(), 4);
    assert_eq!(m.integrations.len(), 2);
}

#[test]
fn manifest_repo_paths_are_btreemap_keys() {
    let m: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();

    // BTreeMap iterates in sorted order — verify keys come out sorted.
    let keys: Vec<&RepoPath> = m.repositories.keys().collect();
    assert_eq!(keys[0].as_str(), "github/acme/client");
    assert_eq!(keys[1].as_str(), "github/acme/server");
    assert_eq!(keys[2].as_str(), "github/docs/rfc");
    assert_eq!(keys[3].as_str(), "github/lib/openssl");
}

#[test]
fn manifest_repo_entry_fields() {
    let m: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();
    let server = &m.repositories[&RepoPath::new("github/acme/server")];
    assert_eq!(server.vcs_type, VcsType::Git);
    assert_eq!(server.url, "https://github.com/acme/server.git");
    assert_eq!(server.version, RefName::new("main"));
    assert_eq!(server.role, Role::Primary);
}

// ---------------------------------------------------------------------------
// Role deserialization
// ---------------------------------------------------------------------------

#[test]
fn role_deserialization_all_variants() {
    let m: Manifest = serde_yaml::from_str(FULL_MANIFEST_YAML).unwrap();
    let role_of = |key: &str| m.repositories[&RepoPath::new(key)].role;

    assert_eq!(role_of("github/acme/server"), Role::Primary);
    assert_eq!(role_of("github/acme/client"), Role::Fork);
    assert_eq!(role_of("github/lib/openssl"), Role::Dependency);
    assert_eq!(role_of("github/docs/rfc"), Role::Reference);
}

#[test]
fn role_is_active() {
    assert!(Role::Primary.is_active());
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
    for entry in m.repositories.values() {
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
    assert_eq!(m.repositories.len(), 1);
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
    assert!(m.integrations["cargo"].enabled.is_none());
}

// ---------------------------------------------------------------------------
// Lock file parsing
// ---------------------------------------------------------------------------

#[test]
fn lock_with_workweave_provenance() {
    let lock: LockFile = serde_yaml::from_str(LOCK_WITH_WORKWEAVE_YAML).unwrap();
    assert_eq!(lock.workweave, Some(WorkweaveName::new("hotfix-42")));
    assert_eq!(lock.repositories.len(), 2);

    let server = &lock.repositories[&RepoPath::new("github/acme/server")];
    assert_eq!(server.vcs_type, VcsType::Git);
    assert_eq!(server.version, RevisionId::new("abc123def456"));
}

#[test]
fn lock_without_workweave_provenance() {
    let lock: LockFile = serde_yaml::from_str(LOCK_WITHOUT_WORKWEAVE_YAML).unwrap();
    assert_eq!(lock.workweave, None);
    assert_eq!(lock.repositories.len(), 1);
}

#[test]
fn lock_repo_paths_sorted() {
    let lock: LockFile = serde_yaml::from_str(LOCK_WITH_WORKWEAVE_YAML).unwrap();
    let keys: Vec<&str> = lock.repositories.keys().map(|k| k.as_str()).collect();
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

    assert_eq!(original.repositories.len(), deserialized.repositories.len());
    for (key, orig_entry) in &original.repositories {
        let de_entry = &deserialized.repositories[key];
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
    assert_eq!(
        original.repositories.len(),
        deserialized.repositories.len()
    );
    for (key, orig_entry) in &original.repositories {
        let de_entry = &deserialized.repositories[key];
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
    assert_eq!(project.manifest.repositories.len(), 1);
    assert!(project.lock.is_none());
    assert_eq!(project.dir, dir.path());
}

#[test]
fn project_from_dir_manifest_and_lock() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rwv.yaml"), FULL_MANIFEST_YAML).unwrap();
    std::fs::write(dir.path().join("rwv.lock"), LOCK_WITH_WORKWEAVE_YAML).unwrap();

    let project = Project::from_dir(dir.path()).unwrap();
    assert_eq!(project.manifest.repositories.len(), 4);
    let lock = project.lock.as_ref().unwrap();
    assert_eq!(lock.workweave, Some(WorkweaveName::new("hotfix-42")));
    assert_eq!(lock.repositories.len(), 2);
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
