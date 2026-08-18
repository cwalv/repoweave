// ===========================================================================
// npm-workspaces
// ===========================================================================

use super::*;

#[test]
fn auto_detects_repos_with_package_json() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Create repos: two with package.json, one without
    touch(root, "github/acme/server/package.json");
    touch(root, "github/acme/web/package.json");
    touch(root, "github/acme/docs/README.md");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
        ("github/acme/docs", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = NpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
    let workspaces = parsed["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 2);
    assert!(workspaces.contains(&serde_json::json!("github/acme/server")));
    assert!(workspaces.contains(&serde_json::json!("github/acme/web")));
    // docs should NOT be included (no package.json)
    assert!(!workspaces.contains(&serde_json::json!("github/acme/docs")));
}

#[test]
fn generates_root_package_json_with_workspaces_array() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/chatly/protocol/package.json");
    touch(root, "github/chatly/server/package.json");
    touch(root, "github/chatly/web/package.json");

    let manifest = make_manifest(vec![
        ("github/chatly/protocol", Role::Owned),
        ("github/chatly/server", Role::Owned),
        ("github/chatly/web", Role::Fork),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = NpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
    // name is DefaultOnly — on a fresh file, it is set from the project name.
    assert_eq!(parsed["name"], "test-project");
    assert_eq!(parsed["private"], true);
    let workspaces = parsed["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 3);
    // Should be sorted
    assert_eq!(workspaces[0], "github/chatly/protocol");
    assert_eq!(workspaces[1], "github/chatly/server");
    assert_eq!(workspaces[2], "github/chatly/web");
}

#[test]
fn excludes_reference_repos() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");
    touch(root, "github/acme/reference-lib/package.json");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/reference-lib", Role::Reference),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = NpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
    let workspaces = parsed["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0], "github/acme/server");
}

#[test]
fn multi_package_repo_expands_to_prefixed_globs() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // A multi-package repo: root package.json declares its own workspaces.
    write_file(
        root,
        "github/acme/mono/package.json",
        r#"{"name":"mono","private":true,"workspaces":["packages/*","./clients/ts"]}"#,
    );
    // A plain single-package repo alongside it.
    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![
        ("github/acme/mono", Role::Owned),
        ("github/acme/server", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = NpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
    let workspaces = parsed["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 3);
    // Globs are repo-prefixed; leading "./" in member globs is normalized.
    assert!(workspaces.contains(&serde_json::json!("github/acme/mono/packages/*")));
    assert!(workspaces.contains(&serde_json::json!("github/acme/mono/clients/ts")));
    // The multi-package repo root itself is NOT an entry.
    assert!(!workspaces.contains(&serde_json::json!("github/acme/mono")));
    // Single-package repo keeps current behavior.
    assert!(workspaces.contains(&serde_json::json!("github/acme/server")));
}

#[test]
fn multi_package_repo_object_form_workspaces_expands() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "github/acme/mono/package.json",
        r#"{"private":true,"workspaces":{"packages":["packages/*"],"nohoist":["**/x"]}}"#,
    );

    let manifest = make_manifest(vec![("github/acme/mono", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = NpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
    let workspaces = parsed["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0], "github/acme/mono/packages/*");
}

#[test]
fn deactivation_strips_author_keys_preserves_default_only_keys() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // name and private are Ownership::DefaultOnly — they are never stripped
    // on deactivate. Only workspaces (Author) is removed. The file retains
    // name and private so it is NOT deleted.
    write_file(
        root,
        "package.json",
        r#"{"x-repoweave":{"managed":true},"name":"repoweave","private":true,"workspaces":[]}"#,
    );
    assert!(root.join("package.json").exists());

    let integration = NpmWorkspaces;
    integration.deactivate(root).unwrap();

    // File must survive (name + private remain).
    assert!(
        root.join("package.json").exists(),
        "package.json must not be deleted — name and private (DefaultOnly) remain"
    );
    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    // Author keys stripped.
    assert!(
        parsed.get("workspaces").is_none(),
        "workspaces should be stripped"
    );
    assert!(
        parsed.get("x-repoweave").is_none(),
        "marker should be stripped"
    );
    // DefaultOnly keys preserved.
    assert_eq!(
        parsed["name"], "repoweave",
        "name should survive (DefaultOnly)"
    );
    assert_eq!(
        parsed["private"], true,
        "private should survive (DefaultOnly)"
    );
}

#[test]
fn deactivation_removes_package_json_when_only_author_keys_present() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // A file with only the marker + workspaces (no name/private, no user fields).
    // After stripping the Author key, nothing remains → file deleted.
    write_file(
        root,
        "package.json",
        r#"{"x-repoweave":{"managed":true},"workspaces":["github/acme/server"]}"#,
    );
    assert!(root.join("package.json").exists());

    let integration = NpmWorkspaces;
    integration.deactivate(root).unwrap();
    assert!(
        !root.join("package.json").exists(),
        "package.json should be deleted when only Author keys (workspaces) remain"
    );
}

#[test]
fn deactivation_preserves_handwritten_package_json() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // A hand-written package.json without the generated name should NOT be removed
    write_file(
        root,
        "package.json",
        r#"{"name":"my-app","private":true,"workspaces":["packages/*"]}"#,
    );
    assert!(root.join("package.json").exists());

    let integration = NpmWorkspaces;
    integration.deactivate(root).unwrap();
    assert!(root.join("package.json").exists());
}

/// Activating over a package.json that already contains user-authored fields
/// (scripts, devDependencies, engines, version, etc.) must preserve those
/// fields while overwriting the three rwv-owned keys.
#[test]
fn activate_preserves_user_fields_in_existing_package_json() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Repo with a package.json so the integration detects it.
    touch(root, "github/acme/server/package.json");

    // Pre-existing workspace-root package.json that was already activated
    // (carries the x-repoweave marker) plus user-authored fields.
    write_file(
        root,
        "package.json",
        r#"{
  "x-repoweave": {"managed": true},
  "name": "repoweave",
  "private": true,
  "workspaces": [],
  "scripts": {
    "ci": "npm run build && npm test"
  },
  "devDependencies": {
    "typescript": "^5.0.0"
  },
  "engines": {
    "node": ">=18"
  },
  "version": "0.1.0"
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // name and private are DefaultOnly — existing value is preserved, not overwritten.
    assert_eq!(
        parsed["name"], "repoweave",
        "name (DefaultOnly) should be preserved from the existing file"
    );
    assert_eq!(
        parsed["private"], true,
        "private (DefaultOnly) should be preserved"
    );
    // x-repoweave marker must be present.
    assert!(
        parsed.get("x-repoweave").is_some(),
        "x-repoweave marker should be present after activate"
    );
    let workspaces = parsed["workspaces"]
        .as_array()
        .expect("workspaces should be an array");
    assert!(
        workspaces
            .iter()
            .any(|w| w.as_str() == Some("github/acme/server")),
        "workspaces should contain the detected repo; got: {workspaces:?}"
    );

    // User-authored fields must survive.
    assert_eq!(
        parsed["scripts"]["ci"], "npm run build && npm test",
        "scripts.ci should survive activate"
    );
    assert_eq!(
        parsed["devDependencies"]["typescript"], "^5.0.0",
        "devDependencies should survive activate"
    );
    assert_eq!(
        parsed["engines"]["node"], ">=18",
        "engines should survive activate"
    );
    assert_eq!(
        parsed["version"], "0.1.0",
        "version should survive activate"
    );
}

/// Activating multiple times in a row must not clobber user fields.
/// This is the real-world scenario: repeated `rwv activate` runs (e.g.,
/// after adding a repo) must be idempotent with respect to user scripts.
#[test]
fn activate_is_idempotent_with_user_scripts() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");

    // First activate from scratch — no pre-existing file.
    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();

    // Simulate user adding a ci script after first activate.
    let pkg_path = root.join("package.json");
    let mut pkg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pkg_path).unwrap()).unwrap();
    pkg["scripts"] = serde_json::json!({"ci": "npm test"});
    std::fs::write(
        &pkg_path,
        serde_json::to_string_pretty(&pkg).unwrap() + "\n",
    )
    .unwrap();

    // Second activate — should preserve the ci script.
    NpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(&pkg_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        parsed["scripts"]["ci"], "npm test",
        "ci script should survive a second activate"
    );
    // name is DefaultOnly — on a fresh file it was set to the project name;
    // subsequent activates preserve the existing value unchanged.
    assert_eq!(parsed["name"], "test-project");
}

/// Deactivating a package.json that carries user scripts alongside rwv-owned
/// keys strips only the Author keys (workspaces) and the marker.
/// name and private are DefaultOnly — they survive deactivation.
#[test]
fn deactivation_strips_rwv_keys_but_preserves_user_fields() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Marker is x-repoweave. The file has user-authored fields (scripts,
    // version) that must survive deactivation. name and private are
    // DefaultOnly and also survive.
    write_file(
        root,
        "package.json",
        r#"{
  "x-repoweave": {"managed": true},
  "name": "repoweave",
  "private": true,
  "workspaces": ["github/acme/server"],
  "scripts": {
    "ci": "npm run build && npm test"
  },
  "version": "0.1.0"
}"#,
    );

    NpmWorkspaces.deactivate(root).unwrap();

    // File must still exist (name, private, scripts, version all remain).
    assert!(
        root.join("package.json").exists(),
        "package.json should not be deleted when non-Author fields remain"
    );

    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // Author key and marker must be gone.
    assert!(
        parsed.get("x-repoweave").is_none(),
        "x-repoweave marker should be stripped on deactivate"
    );
    assert!(
        parsed.get("workspaces").is_none(),
        "workspaces (Author) should be stripped on deactivate"
    );

    // DefaultOnly keys survive — user may have intentionally set them.
    assert_eq!(
        parsed["name"], "repoweave",
        "name (DefaultOnly) should survive deactivate"
    );
    assert_eq!(
        parsed["private"], true,
        "private (DefaultOnly) should survive deactivate"
    );

    // User fields must remain.
    assert_eq!(
        parsed["scripts"]["ci"], "npm run build && npm test",
        "scripts.ci should survive deactivate"
    );
    assert_eq!(
        parsed["version"], "0.1.0",
        "version should survive deactivate"
    );
}

#[cfg(unix)]
#[test]
fn check_warns_when_npm_not_on_path() {
    let absent = doctor_json_on_tool_only_path(
        "npm-workspaces",
        "github/acme/server/package.json",
        "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
        &[],
    );
    let present = doctor_json_on_tool_only_path(
        "npm-workspaces",
        "github/acme/server/package.json",
        "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
        &["npm"],
    );

    tool_missing_fires_then_clears(&absent, &present, "npm-workspaces", "npm");
}

// -----------------------------------------------------------------------
// Regression tests — DefaultOnly name/private
// -----------------------------------------------------------------------

/// Regression: a tmuxcc-style package.json with a real name and
/// custom scripts must survive `merge_activate` with name and scripts intact.
///
/// Before this change, name was Ownership::Author, so activate always
/// overwrote `name` with the hardcoded literal "repoweave" — trashing e.g.
/// `name: "tmuxcc"` in a tmuxcc workweave.
#[test]
fn regression_name_and_scripts_survive_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/cwalv/tmuxcc-daemon/package.json");
    touch(root, "github/cwalv/tmuxcc-client/package.json");

    // tmuxcc-style: pre-existing marked file with a real project name and
    // custom scripts. This simulates what a tmuxcc workweave looks like.
    let original = r#"{
  "x-repoweave": {"managed": true},
  "name": "tmuxcc",
  "private": true,
  "workspaces": ["github/cwalv/tmuxcc-daemon"],
  "scripts": {
    "build": "tsc -b",
    "test": "node --test"
  }
}"#;
    write_file(root, "package.json", original);

    let manifest = make_manifest(vec![
        ("github/cwalv/tmuxcc-daemon", Role::Owned),
        ("github/cwalv/tmuxcc-client", Role::Owned),
    ]);
    // Project name is different from the name in the file — must NOT overwrite.
    let project = ProjectName::new("tmuxcc").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // The core regression: name must NOT be overwritten.
    assert_eq!(
        parsed["name"], "tmuxcc",
        "name must survive activate — was 'tmuxcc', must not become 'repoweave'"
    );

    // Custom scripts must survive untouched (untracked field).
    assert_eq!(
        parsed["scripts"]["build"], "tsc -b",
        "scripts.build must survive activate"
    );
    assert_eq!(
        parsed["scripts"]["test"], "node --test",
        "scripts.test must survive activate"
    );

    // workspaces (Author) is updated normally.
    let ws = parsed["workspaces"].as_array().unwrap();
    assert!(ws
        .iter()
        .any(|w| w.as_str() == Some("github/cwalv/tmuxcc-daemon")));
    assert!(ws
        .iter()
        .any(|w| w.as_str() == Some("github/cwalv/tmuxcc-client")));
}

/// Greenfield test: fresh file gets name set from ctx.project (not "repoweave").
#[test]
fn greenfield_name_set_from_context_project_name() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/api/package.json");

    let manifest = make_manifest(vec![("github/acme/api", Role::Owned)]);
    let project = ProjectName::new("my-cool-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(
        parsed["name"], "my-cool-project",
        "greenfield name must come from ctx.project, not the hardcoded literal"
    );
    assert_eq!(
        parsed["private"], true,
        "greenfield private must be set to true"
    );
}

/// DefaultOnly survival test: user-set `private: false` must survive activate.
#[test]
fn default_only_private_false_survives_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/api/package.json");

    // Pre-existing file with marker and user-set `private: false`.
    write_file(
        root,
        "package.json",
        r#"{
  "x-repoweave": {"managed": true},
  "name": "acme-workspace",
  "private": false,
  "workspaces": ["github/acme/api"]
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/api", Role::Owned)]);
    let project = ProjectName::new("acme").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(
        parsed["private"], false,
        "private: false set by user must survive activate (DefaultOnly must NOT overwrite)"
    );
}

// -----------------------------------------------------------------------
// npm-workspaces — RED scenarios (turned green by C4)
// -----------------------------------------------------------------------
//
// Scenarios 1 and 3 partly overlap with the precedent tests above; they
// are restated here so each scenario has a single set of acceptance
// assertions.
//
// Scenario 2 (object-form workspaces + nohoist) is the data-loss
// regression: RED today vs current :44.
//
// The marker migration (`name`-squat → `x-repoweave`) is part of C4. Until
// C4, the marker probe is `x-repoweave`; current code uses `name`. These
// are RED against the marker.

/// Activate over a real app's package.json (array workspaces).
/// User scripts / engines / devDependencies / packageManager survive.
#[test]
fn s6_npm_1_activate_array_workspaces_preserves_user_fields() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");
    touch(root, "github/acme/web/package.json");

    write_file(
        root,
        "package.json",
        r#"{
  "name": "asupersync",
  "private": true,
  "scripts": {
    "build:wasm": "wasm-pack build",
    "build:packages": "tsc -b packages",
    "validate:next-consumer": "node scripts/validate-next.mjs"
  },
  "devDependencies": {
    "typescript": "^5.5.0"
  },
  "engines": {
    "node": ">=18"
  },
  "packageManager": "npm@10.5.0"
}"#,
    );

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let path = root.join("package.json");
    contract::assert_activate_preserves_foreign(
        &path,
        || {
            NpmWorkspaces.activate(&ctx).unwrap();
        },
        &[
            contract::json_probe("private=true", |v| v["private"].as_bool() == Some(true)),
            contract::json_probe("workspaces is sorted array w/ server+web", |v| {
                let arr = v["workspaces"].as_array();
                match arr {
                    Some(a) => {
                        a.iter().any(|w| w.as_str() == Some("github/acme/server"))
                            && a.iter().any(|w| w.as_str() == Some("github/acme/web"))
                    }
                    None => false,
                }
            }),
        ],
        // C4 migrates the marker to `x-repoweave`.
        &contract::json_probe("x-repoweave marker", |v| {
            v.get("x-repoweave").is_some() || v["x-repoweave"]["managed"].as_bool() == Some(true)
        }),
        &[
            "build:wasm",
            "validate:next-consumer",
            "\"typescript\": \"^5.5.0\"",
            "\"node\": \">=18\"",
            "packageManager",
            "npm@10.5.0",
        ],
    );
}

/// Activate over workspaces OBJECT form with nohoist
/// (the data-loss regression test). Current :44 flattens the object.
#[test]
fn s6_npm_2_activate_preserves_object_form_workspaces_nohoist() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/mobile/package.json");
    touch(root, "github/acme/server/package.json");

    // Seed represents a previously-activated state: x-repoweave marker present,
    // workspaces in object form (packages managed by rwv, nohoist user-authored).
    write_file(
        root,
        "package.json",
        r#"{
  "x-repoweave": { "managed": true },
  "name": "happy",
  "private": true,
  "workspaces": {
    "packages": ["apps/*"],
    "nohoist": ["**/react-native", "**/react-native/**"]
  },
  "scripts": {
    "env:dev": "env-cmd -e dev",
    "env:prod": "env-cmd -e prod"
  }
}"#,
    );

    let manifest = make_manifest(vec![
        ("github/acme/mobile", Role::Owned),
        ("github/acme/server", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // workspaces MUST remain an object (not flatten to an array).
    assert!(
        parsed["workspaces"].is_object(),
        "workspaces must remain an object form; got: {}",
        parsed["workspaces"]
    );
    let packages = parsed["workspaces"]["packages"].as_array().unwrap();
    assert!(
        packages
            .iter()
            .any(|p| p.as_str() == Some("github/acme/mobile")),
        "workspaces.packages should contain detected member; got: {packages:?}"
    );
    assert!(
        packages
            .iter()
            .any(|p| p.as_str() == Some("github/acme/server")),
        "workspaces.packages should contain detected member; got: {packages:?}"
    );
    // The data-loss bit: nohoist must survive verbatim.
    let nohoist = parsed["workspaces"]["nohoist"].as_array().unwrap();
    assert_eq!(
        nohoist.len(),
        2,
        "nohoist entries should survive byte-for-byte"
    );
    assert_eq!(nohoist[0].as_str(), Some("**/react-native"));
    assert_eq!(nohoist[1].as_str(), Some("**/react-native/**"));
    // User scripts survive.
    assert_eq!(parsed["scripts"]["env:dev"], "env-cmd -e dev");
    assert_eq!(parsed["scripts"]["env:prod"], "env-cmd -e prod");
}

/// Re-activate idempotent on user content (preserve_order).
/// Add a third member; only mutation is the added workspaces entry.
#[test]
fn s6_npm_3_reactivate_idempotent_preserve_order() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Scenario-1 result starting point: pre-activated file with marker
    // + array workspaces + user scripts/engines/packageManager.
    write_file(
        root,
        "package.json",
        r#"{
  "x-repoweave": { "managed": true },
  "private": true,
  "workspaces": ["github/acme/server", "github/acme/web"],
  "scripts": {
    "zzz-last": "echo z",
    "aaa-first": "echo a",
    "build:wasm": "wasm-pack build"
  },
  "engines": { "node": ">=18" },
  "packageManager": "npm@10.5.0"
}"#,
    );
    touch(root, "github/acme/server/package.json");
    touch(root, "github/acme/web/package.json");
    touch(root, "github/acme/api/package.json");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
        ("github/acme/api", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();
    let after_first = std::fs::read_to_string(root.join("package.json")).unwrap();

    // Re-activate must be idempotent (same ctx, same result).
    NpmWorkspaces.activate(&ctx).unwrap();
    let after_second = std::fs::read_to_string(root.join("package.json")).unwrap();
    assert_eq!(
        after_first, after_second,
        "second activate must be byte-identical to the first; \
             diff:\nfirst:\n{after_first}\nsecond:\n{after_second}"
    );

    // Order-preservation: user's scripts must NOT be alphabetized
    // (preserve_order). zzz-last appears before aaa-first in the seed.
    let zzz_pos = after_first
        .find("zzz-last")
        .expect("zzz-last must be present");
    let aaa_pos = after_first
        .find("aaa-first")
        .expect("aaa-first must be present");
    assert!(
        zzz_pos < aaa_pos,
        "scripts must preserve user insertion order (zzz before aaa); got:\n{after_first}"
    );
}

/// Deactivate strips Author keys (workspaces) and marker,
/// preserves DefaultOnly keys (name, private) and user content, deletes
/// the lockfile (gated on marker), leaves file only if non-Author content
/// remains.
///
/// Sub-case (a): marked file + user fields + rwv-generated lockfile.
/// Sub-case (b): hand-written file, no marker — file untouched.
#[test]
fn s6_npm_4_deactivate_handles_marker_and_lockfile() {
    // Sub-case (a): rwv-owned file (marker + name + private + workspaces)
    // + user fields + rwv-generated package-lock.json.
    let tmp_a = common::tempdir().unwrap();
    let root_a = tmp_a.path();
    write_file(
        root_a,
        "package.json",
        r#"{
  "x-repoweave": { "managed": true },
  "name": "repoweave",
  "private": true,
  "workspaces": ["github/acme/server"],
  "scripts": { "validate:next-consumer": "node scripts/validate-next.mjs" },
  "engines": { "node": ">=18" },
  "packageManager": "npm@10.5.0"
}"#,
    );
    write_file(root_a, "package-lock.json", r#"{"lockfileVersion": 3}"#);

    NpmWorkspaces.deactivate(root_a).unwrap();

    assert!(
        root_a.join("package.json").exists(),
        "package.json must still exist (user fields and DefaultOnly keys remain)"
    );
    let content = std::fs::read_to_string(root_a.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(parsed.get("x-repoweave").is_none(), "marker stripped");
    assert!(
        parsed.get("workspaces").is_none(),
        "workspaces (Author) stripped"
    );
    // name and private are DefaultOnly — they are NOT stripped.
    assert_eq!(parsed["name"], "repoweave", "name (DefaultOnly) survives");
    assert_eq!(parsed["private"], true, "private (DefaultOnly) survives");
    assert_eq!(
        parsed["scripts"]["validate:next-consumer"], "node scripts/validate-next.mjs",
        "user scripts survive"
    );
    assert_eq!(
        parsed["packageManager"], "npm@10.5.0",
        "packageManager survives"
    );
    assert!(
        !root_a.join("package-lock.json").exists(),
        "lockfile must be removed on deactivate (gated on marker)"
    );

    // Sub-case (b): hand-written file with NO marker → leave alone, and
    // leave its lockfile alone (lockfile removal is gated on marker).
    let tmp_b = common::tempdir().unwrap();
    let root_b = tmp_b.path();
    let user_pkg = r#"{
  "name": "my-app",
  "private": true,
  "workspaces": ["packages/*"]
}"#;
    write_file(root_b, "package.json", user_pkg);
    write_file(root_b, "package-lock.json", r#"{"lockfileVersion": 2}"#);

    NpmWorkspaces.deactivate(root_b).unwrap();

    assert!(
        root_b.join("package.json").exists(),
        "hand-written package.json must be preserved"
    );
    let after = std::fs::read_to_string(root_b.join("package.json")).unwrap();
    assert_eq!(after, user_pkg, "hand-written file must be untouched");
    assert!(
        root_b.join("package-lock.json").exists(),
        "hand-written package-lock.json must survive (no marker → no removal)"
    );
}
