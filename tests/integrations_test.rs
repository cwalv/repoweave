//! E2E tests for built-in integrations.
//!
//! Each integration is tested for:
//! 1. Auto-detection of relevant repos
//! 2. File generation matching the spec in docs/integrations.md
//! 3. Reference repos excluded from generated files
//! 4. Deactivation cleanup
//! 5. Check warnings (e.g., missing tools)
//!
//! # RED-first scenarios — fo-cnpjy.14 (TDD anchor)
//!
//! The §6 scenarios from
//! `projects/foundations/docs/repoweave/integration-ownership/plan.md` are
//! realized here as RED-first tests, per [[feedback_no_workaround_assertions]].
//! Tests that assert behavior the current code does NOT yet implement are
//! marked `#[ignore = "RED: turned green by fo-cnpjy.N"]`. The port author
//! (npm: C4, vscode: C5, cargo: C7, uv: C9, pnpm: C10, go: C11, static-files:
//! C13) removes the `#[ignore]` attribute when their port lands and the test
//! turns green naturally.
//!
//! **Why `#[ignore]` rather than letting tests fail?** The bead offered both;
//! we picked `#[ignore]` because (a) the port beads land incrementally over
//! Phase 3 and a permanently-red suite would block other beads in the epic
//! from confirming their own greens, and (b) `cargo test -- --ignored`
//! enumerates every RED test in one run, which is the visibility the bead
//! wants for port authors. The `#[ignore]` annotation always names the bead
//! that will flip it.
//!
//! Assertions describe the REAL desired behavior; they are not contorted to
//! stay green against the current broken code (per no-workaround-assertions).
//!
//! The shared common-contract helper lives at `tests/common/contract.rs`.

mod common;

use common::contract;
use repoweave::integration::{Integration, IntegrationContext, Severity};
use repoweave::integrations::*;
use repoweave::manifest::{
    IntegrationConfig, Manifest, ProjectName, RepoPath, Role, WorkweaveConfig,
};
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

// ===========================================================================
// Test helpers
// ===========================================================================

/// Build a Manifest with the given repo entries and no integration config overrides.
fn make_manifest(repos: Vec<(&str, Role)>) -> Manifest {
    let mut yaml = String::from("repositories:\n");
    for (path, role) in &repos {
        let last = path.split('/').next_back().unwrap();
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: https://github.com/test/{last}.git\n    version: main\n    role: {}\n",
            role.as_str()
        ));
    }
    Manifest::from_yaml_str(&yaml).unwrap()
}

/// Build an IntegrationContext from parts.
/// Both output_dir and workspace_root default to `root`.
fn make_ctx<'a>(
    root: &'a Path,
    project: &'a ProjectName,
    manifest: &'a Manifest,
    config: &'a IntegrationConfig,
    cache: &'a HashMap<String, Vec<String>>,
) -> IntegrationContext<'a> {
    IntegrationContext {
        output_dir: root,
        workspace_root: root,
        project,
        repos: manifest
            .iter_entries()
            .map(|(rp, e)| (rp.clone(), e.clone()))
            .collect(),
        config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
        detection_cache: cache,
        workweave: None,
    }
}

/// Build an IntegrationContext with an attached workweave config. Used by
/// static-files collision tests (rwv-c5h regression — fo-cnpjy.13).
fn make_ctx_with_workweave<'a>(
    root: &'a Path,
    project: &'a ProjectName,
    manifest: &'a Manifest,
    config: &'a IntegrationConfig,
    cache: &'a HashMap<String, Vec<String>>,
    workweave: &'a WorkweaveConfig,
) -> IntegrationContext<'a> {
    IntegrationContext {
        output_dir: root,
        workspace_root: root,
        project,
        repos: manifest
            .iter_entries()
            .map(|(rp, e)| (rp.clone(), e.clone()))
            .collect(),
        config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
        detection_cache: cache,
        workweave: Some(workweave),
    }
}

/// Create a file inside a temp dir at the given relative path, including parent dirs.
fn touch(root: &Path, relative: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, "").unwrap();
}

/// Create a file inside a temp dir at the given relative path with content.
fn write_file(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
}

// ===========================================================================
// npm-workspaces
// ===========================================================================

mod npm_workspaces {
    use super::*;

    #[test]
    fn auto_detects_repos_with_package_json() {
        let tmp = TempDir::new().unwrap();
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
        let project = ProjectName::new("test-project");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/package.json");
        touch(root, "github/chatly/server/package.json");
        touch(root, "github/chatly/web/package.json");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Owned),
            ("github/chatly/server", Role::Owned),
            ("github/chatly/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
        assert_eq!(parsed["name"], "repoweave");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/reference-lib/package.json");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
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
    fn deactivation_removes_package_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Marker is x-repoweave (new sentinel); name/private/workspaces are
        // the owned keys. When nothing else remains after stripping, the file
        // must be deleted.
        write_file(
            root,
            "package.json",
            r#"{"x-repoweave":{"managed":true},"name":"repoweave","private":true,"workspaces":[]}"#,
        );
        assert!(root.join("package.json").exists());

        let integration = NpmWorkspaces;
        integration.deactivate(root).unwrap();
        assert!(!root.join("package.json").exists());
    }

    #[test]
    fn deactivation_preserves_handwritten_package_json() {
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
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
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        NpmWorkspaces.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // rwv-owned keys must be set/updated.
        assert_eq!(
            parsed["name"], "repoweave",
            "name should be the rwv sentinel"
        );
        assert_eq!(parsed["private"], true, "private should remain true");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        // First activate from scratch — no pre-existing file.
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
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
        assert_eq!(parsed["name"], "repoweave");
    }

    /// Deactivating a package.json that carries user scripts (scripts, engines,
    /// devDependencies) alongside the rwv-owned keys must strip only the
    /// three rwv-owned keys and leave the rest on disk.
    #[test]
    fn deactivation_strips_rwv_keys_but_preserves_user_fields() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Marker is x-repoweave. The file has user-authored fields (scripts,
        // version) that must survive deactivation.
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

        // File must still exist because there are user-authored fields.
        assert!(
            root.join("package.json").exists(),
            "package.json should not be deleted when user fields remain"
        );

        let content = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // rwv-owned keys must be gone.
        assert!(
            parsed.get("x-repoweave").is_none(),
            "x-repoweave marker should be stripped on deactivate"
        );
        assert!(
            parsed.get("name").is_none(),
            "name should be stripped on deactivate"
        );
        assert!(
            parsed.get("private").is_none(),
            "private should be stripped on deactivate"
        );
        assert!(
            parsed.get("workspaces").is_none(),
            "workspaces should be stripped on deactivate"
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

    #[test]
    fn check_warns_when_npm_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        let issues = integration.check(&ctx).unwrap();
        // We can't guarantee npm is or isn't on PATH in CI,
        // but we can verify the check runs without error.
        // If npm is not on PATH, there should be a warning.
        if which::which("npm").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("npm")));
        } else {
            assert!(issues.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // §6 npm-workspaces — RED scenarios (fo-cnpjy.14 → green by C4)
    // -----------------------------------------------------------------------
    //
    // Plan §6 npm scenarios 1–4. Scenarios 1 and 3 partly overlap with the
    // existing precedent tests above; we restate them in the §6 shape so the
    // port author has a single set of acceptance assertions per scenario.
    //
    // Scenario 2 (object-form workspaces + nohoist) is the data-loss
    // regression: RED today vs current :44.
    //
    // The marker migration (`name`-squat → `x-repoweave`) is part of C4. Until
    // C4, the marker probe is `x-repoweave`; current code uses `name`. These
    // are RED against the marker.

    /// §6.npm.1 — Activate over a real app's package.json (array workspaces).
    /// User scripts / engines / devDependencies / packageManager survive.
    #[test]
    fn s6_npm_1_activate_array_workspaces_preserves_user_fields() {
        let tmp = TempDir::new().unwrap();
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
        let project = ProjectName::new("test-project");
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
                v.get("x-repoweave").is_some()
                    || v["x-repoweave"]["managed"].as_bool() == Some(true)
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

    /// §6.npm.2 — Activate over workspaces OBJECT form with nohoist
    /// (the data-loss regression test). Current :44 flattens the object.
    #[test]
    fn s6_npm_2_activate_preserves_object_form_workspaces_nohoist() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/mobile/package.json");
        touch(root, "github/acme/server/package.json");

        write_file(
            root,
            "package.json",
            r#"{
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
        let project = ProjectName::new("test-project");
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
        assert_eq!(nohoist.len(), 2, "nohoist entries should survive byte-for-byte");
        assert_eq!(nohoist[0].as_str(), Some("**/react-native"));
        assert_eq!(nohoist[1].as_str(), Some("**/react-native/**"));
        // User scripts survive.
        assert_eq!(parsed["scripts"]["env:dev"], "env-cmd -e dev");
        assert_eq!(parsed["scripts"]["env:prod"], "env-cmd -e prod");
    }

    /// §6.npm.3 — Re-activate idempotent on user content (preserve_order).
    /// Add a third member; only mutation is the added workspaces entry.
    #[test]
    fn s6_npm_3_reactivate_idempotent_preserve_order() {
        let tmp = TempDir::new().unwrap();
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
        let project = ProjectName::new("test-project");
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
        let zzz_pos = after_first.find("zzz-last").expect("zzz-last must be present");
        let aaa_pos = after_first
            .find("aaa-first")
            .expect("aaa-first must be present");
        assert!(
            zzz_pos < aaa_pos,
            "scripts must preserve user insertion order (zzz before aaa); got:\n{after_first}"
        );
    }

    /// §6.npm.4 — Deactivate strips rwv keys, preserves user content, deletes
    /// lockfile, removes file only if empty.
    ///
    /// Sub-case (a): marked file + user fields + rwv-generated lockfile.
    /// Sub-case (b): hand-written file, no marker — file untouched.
    #[test]
    fn s6_npm_4_deactivate_handles_marker_and_lockfile() {
        // Sub-case (a): rwv-owned file (marker + name + private + workspaces)
        // + user fields + rwv-generated package-lock.json.
        let tmp_a = TempDir::new().unwrap();
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
            "package.json must still exist (user fields remain)"
        );
        let content = std::fs::read_to_string(root_a.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.get("x-repoweave").is_none(), "marker stripped");
        assert!(parsed.get("workspaces").is_none(), "workspaces stripped");
        assert!(parsed.get("private").is_none(), "private stripped");
        assert_eq!(
            parsed["scripts"]["validate:next-consumer"],
            "node scripts/validate-next.mjs",
            "user scripts survive"
        );
        assert_eq!(parsed["packageManager"], "npm@10.5.0", "packageManager survives");
        assert!(
            !root_a.join("package-lock.json").exists(),
            "lockfile must be removed on deactivate (gated on marker)"
        );

        // Sub-case (b): hand-written file with NO marker → leave alone, and
        // leave its lockfile alone (lockfile removal is gated on marker).
        let tmp_b = TempDir::new().unwrap();
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
}

// ===========================================================================
// pnpm-workspaces
// ===========================================================================

mod pnpm_workspaces {
    use super::*;

    #[test]
    fn auto_detects_repos_with_package_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/web/package.json");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
            ("github/acme/docs", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_pnpm_workspace_yaml_with_packages_list() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/package.json");
        touch(root, "github/chatly/server/package.json");
        touch(root, "github/chatly/web/package.json");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Owned),
            ("github/chatly/server", Role::Owned),
            ("github/chatly/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        // Activate now writes the `# managed by repoweave` marker above the
        // packages block — that is the ownership sentinel for the hybrid contract.
        assert!(content.contains("# managed by repoweave"));
        assert!(content.contains("packages:"));
        assert!(content.contains("  - github/chatly/protocol"));
        assert!(content.contains("  - github/chatly/server"));
        assert!(content.contains("  - github/chatly/web"));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/reference-lib/package.json");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(!content.contains("reference-lib"));
    }

    #[test]
    fn deactivation_deletes_fully_rwv_authored_file() {
        // When the file was authored entirely by rwv (only a marker + packages
        // block, nothing user-authored), deactivation should delete it.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pnpm-workspace.yaml",
            "# managed by repoweave\npackages:\n  - foo\n",
        );
        assert!(root.join("pnpm-workspace.yaml").exists());

        let integration = PnpmWorkspaces;
        integration.deactivate(root).unwrap();
        assert!(!root.join("pnpm-workspace.yaml").exists());
    }

    #[test]
    fn deactivation_leaves_hand_owned_file_alone() {
        // A file without the marker was not authored by rwv — leave it alone.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(root, "pnpm-workspace.yaml", "packages:\n  - foo\n");
        assert!(root.join("pnpm-workspace.yaml").exists());

        let integration = PnpmWorkspaces;
        integration.deactivate(root).unwrap();
        // No marker → user took the pen → file must survive.
        assert!(root.join("pnpm-workspace.yaml").exists());
    }

    #[test]
    fn check_warns_when_pnpm_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        let issues = integration.check(&ctx).unwrap();
        if which::which("pnpm").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("pnpm")));
        } else {
            assert!(issues.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // §6 pnpm-workspaces — RED scenarios (fo-cnpjy.14 → green by C10)
    // -----------------------------------------------------------------------
    //
    // Synthetic scenarios (per plan §12.4): no on-disk pnpm-workspace.yaml
    // exists in any weave; the four scenarios use spec idioms (`catalog:`,
    // `overrides:`, `peerDependencyRules:`, `# comments`). default_enabled is
    // false today; tests force it on via `enabled: true` in the config.
    //
    // The pnpm integration uses `default_enabled=false`, but we still call
    // activate/deactivate directly (the integration's own gating logic ignores
    // default_enabled when invoked through trait methods).

    /// §6.pnpm.1 — Activate preserves a user catalog and comment.
    #[test]
    fn s6_pnpm_1_activate_preserves_catalog_and_comments() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "github/acme/server/package.json");

        // Pre-existing user YAML with catalog and a rationale comment.
        write_file(
            root,
            "pnpm-workspace.yaml",
            r#"# shared dependency versions
catalog:
  react: ^18.2.0
  react-dom: ^18.2.0

packages:
  - tools/*
"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let path = root.join("pnpm-workspace.yaml");
        contract::assert_activate_preserves_foreign(
            &path,
            || {
                PnpmWorkspaces.activate(&ctx).unwrap();
            },
            &[contract::substr_probe(
                "server in packages",
                "github/acme/server",
            )],
            &contract::substr_probe("yaml marker", "managed by repoweave"),
            &[
                "# shared dependency versions",
                "catalog:",
                "react: ^18.2.0",
                "react-dom: ^18.2.0",
            ],
        );
    }

    /// §6.pnpm.2 — Deactivate strips `packages:` but keeps `overrides:`.
    /// Regression vs current unconditional remove_file at pnpm_workspaces.rs:33-35.
    #[test]
    fn s6_pnpm_2_deactivate_strips_packages_keeps_overrides() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pnpm-workspace.yaml",
            r#"overrides:
  lodash@<4.17.21: '>=4.17.21'

# managed by repoweave
packages:
  - github/acme/server
"#,
        );

        let path = root.join("pnpm-workspace.yaml");
        contract::assert_deactivate_strips_keeps(
            &path,
            || {
                PnpmWorkspaces.deactivate(root).unwrap();
            },
            &[contract::substr_probe(
                "server entry",
                "github/acme/server",
            )],
            &contract::substr_probe("yaml marker", "managed by repoweave"),
            &[
                "overrides:",
                "lodash@<4.17.21: '>=4.17.21'",
            ],
        );
    }

    /// §6.pnpm.3 — Deactivate deletes a fully-rwv-authored file (no foreign
    /// content). delete-if-empty kicks in.
    ///
    /// Currently GREEN incidentally — current pnpm deactivate is an
    /// unconditional `remove_file`, which happens to satisfy this scenario.
    /// Keep ungated as a regression guard against the C10 port.
    #[test]
    fn s6_pnpm_3_deactivate_deletes_purely_rwv_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pnpm-workspace.yaml",
            r#"# managed by repoweave
packages:
  - github/acme/server
  - github/acme/web
"#,
        );

        let path = root.join("pnpm-workspace.yaml");
        contract::assert_deactivate_deletes_when_only_owned(&path, || {
            PnpmWorkspaces.deactivate(root).unwrap();
        });
    }

    /// §6.pnpm.4 — Activate is comment-safe & idempotent. peerDependencyRules
    /// with an inline comment survives byte-for-byte, even when activate runs
    /// twice with a member added in between.
    #[test]
    fn s6_pnpm_4_activate_idempotent_comments_preserved() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "github/acme/server/package.json");

        write_file(
            root,
            "pnpm-workspace.yaml",
            r#"peerDependencyRules:
  allowedVersions:
    react: '18'  # pin during migration
"#,
        );

        let manifest_one = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();

        // First activate: just server.
        let ctx_one = make_ctx(root, &project, &manifest_one, &config, &cache);
        PnpmWorkspaces.activate(&ctx_one).unwrap();
        let after_first = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();

        // Second activate: server + web.
        touch(root, "github/acme/web/package.json");
        let manifest_two = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let ctx_two = make_ctx(root, &project, &manifest_two, &config, &cache);
        PnpmWorkspaces.activate(&ctx_two).unwrap();
        let after_second = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();

        // Inline comment + peerDependencyRules survive both runs.
        assert!(
            after_first.contains("# pin during migration"),
            "inline comment must survive first activate; got:\n{after_first}"
        );
        assert!(
            after_second.contains("# pin during migration"),
            "inline comment must survive second activate; got:\n{after_second}"
        );
        assert!(after_second.contains("peerDependencyRules:"));
        assert!(after_second.contains("github/acme/server"));
        assert!(after_second.contains("github/acme/web"));

        // No marker duplication: exactly one `# managed by repoweave` line.
        let marker_count = after_second
            .lines()
            .filter(|l| l.trim() == "# managed by repoweave")
            .count();
        assert_eq!(marker_count, 1, "marker must appear exactly once; got:\n{after_second}");

        // No duplicated packages: blocks. Count column-0 `packages:` keys.
        let packages_count = after_second
            .lines()
            .filter(|l| l.starts_with("packages:"))
            .count();
        assert_eq!(
            packages_count, 1,
            "exactly one packages: block; got:\n{after_second}"
        );
    }
}

// ===========================================================================
// go-work
// ===========================================================================

mod go_work {
    use super::*;

    #[test]
    fn auto_detects_repos_with_go_mod() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");
        touch(root, "github/acme/web/go.mod");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
            ("github/acme/docs", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_go_work_with_use_directives() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/chatly/protocol/go.mod",
            "module github.com/chatly/protocol\n\ngo 1.21\n",
        );
        write_file(
            root,
            "github/chatly/server/go.mod",
            "module github.com/chatly/server\n\ngo 1.22\n",
        );

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Owned),
            ("github/chatly/server", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        let expected =
            "go 1.22\n\nuse (\n    ./github/chatly/protocol\n    ./github/chatly/server\n)\n";
        assert_eq!(content, expected);
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");
        touch(root, "github/acme/reference-lib/go.mod");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(!content.contains("reference-lib"));
    }

    #[test]
    fn deactivation_removes_go_work() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(root, "go.work", "go 1.21\n\nuse (\n)\n");
        assert!(root.join("go.work").exists());

        let integration = GoWork;
        integration.deactivate(root).unwrap();
        assert!(!root.join("go.work").exists());
    }

    #[test]
    fn check_warns_when_go_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        let issues = integration.check(&ctx).unwrap();
        if which::which("go").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("go")));
        } else {
            assert!(issues.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // §6 go-work — RED scenarios (fo-cnpjy.14 → green by C11)
    // -----------------------------------------------------------------------
    //
    // Per plan §12.4: real /home/cwa/weaveroot/foundations/go.work uses
    // `go 1.26` + `use(...)` over repoweave + some-go-tool. Member names are
    // illustrative — use `repoweave + some-go-tool` even though actual is
    // ntm/beads/etc.
    //
    // The hand-parse fallback is mandatory; per plan §8 the merge-logic tests
    // must exercise the fallback deterministically. The current impl always
    // overwrites and does not use `go work edit`, so for now we exercise the
    // hand-parse fallback path implicitly (no `go work edit` exists).

    /// §6.go.1 — Adding a repo preserves a hand-authored `replace` directive.
    /// `go 1.26` must NOT be downgraded to `1.21` (the concrete bug).
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.11 (go.work merge port)"]
    fn s6_go_1_add_preserves_replace_and_go_version() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Real existing repos must have go.mod with the version the file uses.
        write_file(
            root,
            "github/cwalv/repoweave/go.mod",
            "module github.com/cwalv/repoweave\n\ngo 1.21\n",
        );
        write_file(
            root,
            "github/cwalv/some-go-tool/go.mod",
            "module github.com/cwalv/some-go-tool\n\ngo 1.21\n",
        );
        write_file(
            root,
            "github/cwalv/another-module/go.mod",
            "module github.com/cwalv/another-module\n\ngo 1.21\n",
        );

        // Pre-existing go.work with go 1.26, two members, a replace + comment.
        write_file(
            root,
            "go.work",
            r#"go 1.26

// managed by repoweave
use (
    ./github/cwalv/repoweave
    ./github/cwalv/some-go-tool
)

// pin local fork for the legacy migration
replace example.com/legacy => ./vendor/legacy
"#,
        );

        let manifest = make_manifest(vec![
            ("github/cwalv/repoweave", Role::Owned),
            ("github/cwalv/some-go-tool", Role::Owned),
            ("github/cwalv/another-module", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        GoWork.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            content.contains("github/cwalv/repoweave"),
            "use must include repoweave; got:\n{content}"
        );
        assert!(
            content.contains("github/cwalv/some-go-tool"),
            "use must include some-go-tool; got:\n{content}"
        );
        assert!(
            content.contains("github/cwalv/another-module"),
            "use must include the newly-added another-module; got:\n{content}"
        );
        assert!(
            content.contains("go 1.26"),
            "go 1.26 must survive (NOT downgraded to 1.21); got:\n{content}"
        );
        assert!(
            !content.contains("go 1.21"),
            "the 1.21 downgrade is the concrete bug; must not appear; got:\n{content}"
        );
        assert!(
            content.contains("replace example.com/legacy => ./vendor/legacy"),
            "replace directive must survive; got:\n{content}"
        );
        assert!(
            content.contains("// pin local fork for the legacy migration"),
            "user comment must survive; got:\n{content}"
        );
    }

    /// §6.go.2 — Removing a repo strips its use entry but keeps toolchain.
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.11 (go.work merge port)"]
    fn s6_go_2_remove_keeps_toolchain_and_godebug() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "github/cwalv/repoweave/go.mod",
            "module github.com/cwalv/repoweave\n\ngo 1.21\n",
        );
        write_file(
            root,
            "github/cwalv/some-go-tool/go.mod",
            "module github.com/cwalv/some-go-tool\n\ngo 1.21\n",
        );

        write_file(
            root,
            "go.work",
            r#"go 1.26

toolchain go1.26.0

godebug default=go1.26

// managed by repoweave
use (
    ./github/cwalv/repoweave
    ./github/cwalv/some-go-tool
    ./github/cwalv/another-module
)
"#,
        );

        // another-module no longer in manifest.
        let manifest = make_manifest(vec![
            ("github/cwalv/repoweave", Role::Owned),
            ("github/cwalv/some-go-tool", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        GoWork.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(content.contains("./github/cwalv/repoweave"));
        assert!(content.contains("./github/cwalv/some-go-tool"));
        assert!(
            !content.contains("./github/cwalv/another-module"),
            "removed member must be gone from use; got:\n{content}"
        );
        assert!(
            content.contains("toolchain go1.26.0"),
            "toolchain must survive; got:\n{content}"
        );
        assert!(
            content.contains("godebug default=go1.26"),
            "godebug must survive; got:\n{content}"
        );
        assert!(
            content.contains("go 1.26"),
            "go version must survive; got:\n{content}"
        );
    }

    /// §6.go.3 — Deactivate strips the use set but keeps replace.
    /// Regression vs current unconditional remove_file at go_work.rs:36-38.
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.11 (go.work merge port)"]
    fn s6_go_3_deactivate_strips_use_keeps_replace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "go.work",
            r#"go 1.26

// managed by repoweave
use (
    ./github/cwalv/repoweave
)

replace example.com/foo => ../foo
"#,
        );

        GoWork.deactivate(root).unwrap();

        assert!(
            root.join("go.work").exists(),
            "go.work must NOT be deleted when foreign content (replace) remains"
        );
        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            !content.contains("./github/cwalv/repoweave"),
            "use entries must be stripped; got:\n{content}"
        );
        assert!(
            !content.contains("// managed by repoweave"),
            "marker must be stripped; got:\n{content}"
        );
        assert!(
            content.contains("go 1.26"),
            "go version must survive; got:\n{content}"
        );
        assert!(
            content.contains("replace example.com/foo => ../foo"),
            "replace must survive; got:\n{content}"
        );
    }

    /// §6.go.4 — Deactivate deletes when only rwv content remained.
    ///
    /// Currently GREEN incidentally — current go.work deactivate is an
    /// unconditional `remove_file`, which happens to satisfy this scenario.
    /// Keep ungated as a regression guard against the C11 port: when C11
    /// switches to strip-not-delete-with-delete-if-empty, this scenario must
    /// still hold (file deleted because the post-strip doc is empty).
    #[test]
    fn s6_go_4_deactivate_deletes_when_only_rwv_content() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "go.work",
            r#"go 1.26

// managed by repoweave
use (
    ./github/cwalv/repoweave
    ./github/cwalv/some-go-tool
)
"#,
        );

        GoWork.deactivate(root).unwrap();

        assert!(
            !root.join("go.work").exists(),
            "go.work must be deleted: only rwv-authored content (go line + use) remains"
        );
    }
}

// ===========================================================================
// uv-workspace
// ===========================================================================

mod uv_workspace {
    use super::*;

    #[test]
    fn auto_detects_repos_with_pyproject_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");
        touch(root, "github/acme/web/pyproject.toml");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
            ("github/acme/docs", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_pyproject_toml_with_uv_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/pyproject.toml");
        touch(root, "github/chatly/server/pyproject.toml");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Owned),
            ("github/chatly/server", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(content.starts_with("# Generated by rwv \u{2014} do not edit\n"));
        assert!(content.contains("[tool.uv.workspace]"));
        assert!(content.contains("\"github/chatly/protocol\""));
        assert!(content.contains("\"github/chatly/server\""));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");
        touch(root, "github/acme/reference-lib/pyproject.toml");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(!content.contains("reference-lib"));
    }

    #[test]
    fn deactivation_removes_pyproject_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pyproject.toml",
            "# Generated by rwv \u{2014} do not edit\n[tool.uv.workspace]\nmembers = []\n",
        );
        assert!(root.join("pyproject.toml").exists());

        let integration = UvWorkspace;
        integration.deactivate(root).unwrap();
        assert!(!root.join("pyproject.toml").exists());
    }

    #[test]
    fn deactivation_preserves_handwritten_pyproject_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // A hand-written pyproject.toml without the generated header should NOT be removed
        write_file(
            root,
            "pyproject.toml",
            "[project]\nname = \"my-project\"\nversion = \"0.1.0\"\n",
        );
        assert!(root.join("pyproject.toml").exists());

        let integration = UvWorkspace;
        integration.deactivate(root).unwrap();
        assert!(root.join("pyproject.toml").exists());
    }

    #[test]
    fn check_warns_when_uv_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        let issues = integration.check(&ctx).unwrap();
        if which::which("uv").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("uv")));
        } else {
            assert!(issues.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // §6 uv-workspace — RED scenarios (fo-cnpjy.14 → green by C9)
    // -----------------------------------------------------------------------
    //
    // Seeds use astral-sh/ruff pyproject.toml idioms (maturin + ruff + black +
    // rooster). Marker = per-key `# managed by rwv` decor on
    // `[tool.uv.workspace].members`. C9 reuses TomlDoc from C7.

    /// §6.uv.1 — Activate preserves a real maturin+ruff root (merge, not clobber).
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.9 (uv merge port)"]
    fn s6_uv_1_activate_preserves_ruff_style_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "github/astral/server/pyproject.toml");
        touch(root, "github/astral/web/pyproject.toml");

        // Ruff-style hand-maintained root with maturin build-backend,
        // [project] with array-of-inline-tables authors, [tool.ruff.lint],
        // [tool.black] with triple-quoted force-exclude, [tool.rooster] with
        // an inline comment.
        write_file(
            root,
            "pyproject.toml",
            r#"[build-system]
requires = ["maturin>=1.7,<2.0"]
build-backend = "maturin"

[project]
name = "acme"
version = "0.1.0"
authors = [
    { name = "Alice", email = "alice@example.com" },
]
classifiers = [
    "Programming Language :: Python :: 3",
]

[project.urls]
Homepage = "https://example.com"

[tool.ruff]
line-length = 100

[tool.ruff.lint]
select = ["E", "F"]

[tool.black]
force-exclude = '''
/(
    | \.eggs
    | build
)/
'''

[tool.rooster]
major_labels = []  # Ruff never uses major bumps
"#,
        );

        let manifest = make_manifest(vec![
            ("github/astral/server", Role::Owned),
            ("github/astral/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let path = root.join("pyproject.toml");
        contract::assert_activate_preserves_foreign(
            &path,
            || {
                UvWorkspace.activate(&ctx).unwrap();
            },
            &[
                contract::substr_probe(
                    "members[server]",
                    "github/astral/server",
                ),
                contract::substr_probe(
                    "members[web]",
                    "github/astral/web",
                ),
            ],
            &contract::substr_probe("toml marker on members", "managed by rwv"),
            &[
                "build-backend = \"maturin\"",
                "name = \"acme\"",
                "{ name = \"Alice\", email = \"alice@example.com\" }",
                "[project.urls]",
                "[tool.ruff.lint]",
                "select = [\"E\", \"F\"]",
                "[tool.black]",
                "force-exclude = '''",
                "build",
                "Ruff never uses major bumps",
                "[tool.rooster]",
            ],
        );

        // The legacy `# Generated by rwv` line MUST NOT be the first line of
        // the file — it would either replace `[build-system]` (the bug) or
        // inject a header into a user file (also rejected per plan §5b).
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.starts_with("# Generated by rwv"),
            "legacy header must not be at line 1 of a user-authored file; \
             got first line: {:?}",
            text.lines().next()
        );
    }

    /// §6.uv.2 — Add a repo: idempotent, only mutates members; user
    /// `[tool.uv.sources]` entries that aren't `{workspace=true}` survive.
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.9 (uv merge port)"]
    fn s6_uv_2_add_member_preserves_user_sources() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "github/astral/server/pyproject.toml");

        // Pre-existing seed: already-activated state with rwv marker decor +
        // user-added [tool.uv.sources] non-workspace entry.
        write_file(
            root,
            "pyproject.toml",
            r#"[project]
name = "acme"
version = "0.1.0"

[tool.ruff]
line-length = 100

[tool.uv.workspace]
# managed by rwv
members = ["github/astral/server"]

[tool.uv.sources]
some-private-lib = { git = "https://example.com/some-private-lib.git" }
"#,
        );

        // Web added to manifest.
        touch(root, "github/astral/web/pyproject.toml");
        let manifest = make_manifest(vec![
            ("github/astral/server", Role::Owned),
            ("github/astral/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        UvWorkspace.activate(&ctx).unwrap();
        let after_first = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();

        // Second activate: must be idempotent.
        UvWorkspace.activate(&ctx).unwrap();
        let after_second = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert_eq!(
            after_first, after_second,
            "second activate must be byte-identical"
        );

        // members contains both, no dup.
        assert!(after_second.contains("github/astral/server"));
        assert!(after_second.contains("github/astral/web"));
        // Exactly one marker decor — no duplication.
        let marker_count = after_second.matches("managed by rwv").count();
        assert_eq!(marker_count, 1, "marker must appear exactly once");
        // User git source survives.
        assert!(
            after_second.contains("some-private-lib"),
            "user [tool.uv.sources] entry must survive; got:\n{after_second}"
        );
        assert!(
            after_second.contains("https://example.com/some-private-lib.git"),
            "user git URL must survive; got:\n{after_second}"
        );
        // User policy survives.
        assert!(after_second.contains("[project]"));
        assert!(after_second.contains("[tool.ruff]"));
    }

    /// §6.uv.3 — Deactivate strips only rwv keys, keeps the manifest.
    /// User non-workspace sources survive.
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.9 (uv strip-not-delete)"]
    fn s6_uv_3_deactivate_strips_keeps_user_manifest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pyproject.toml",
            r#"[build-system]
requires = ["maturin>=1.7,<2.0"]
build-backend = "maturin"

[project]
name = "acme"
version = "0.1.0"

[tool.ruff]
line-length = 100

[tool.uv.workspace]
# managed by rwv
members = ["github/astral/server", "github/astral/web"]

[tool.uv.sources]
# managed by rwv
server = { workspace = true }
some-private-lib = { git = "https://example.com/some-private-lib.git" }
"#,
        );

        UvWorkspace.deactivate(root).unwrap();

        assert!(
            root.join("pyproject.toml").exists(),
            "pyproject.toml must NOT be deleted (foreign content remains)"
        );
        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        // rwv-owned regions gone.
        assert!(
            !content.contains("[tool.uv.workspace]"),
            "[tool.uv.workspace] must be stripped; got:\n{content}"
        );
        assert!(
            !content.contains("workspace = true"),
            "{{workspace=true}} source must be stripped; got:\n{content}"
        );
        assert!(
            !content.contains("managed by rwv"),
            "marker decor must be removed; got:\n{content}"
        );
        // User content survives.
        assert!(content.contains("build-backend = \"maturin\""));
        assert!(content.contains("[project]"));
        assert!(content.contains("[tool.ruff]"));
        // User git source survives.
        assert!(
            content.contains("some-private-lib"),
            "user [tool.uv.sources] entry must survive; got:\n{content}"
        );
        assert!(
            content.contains("https://example.com/some-private-lib.git"),
            "user git URL must survive"
        );
    }

    /// §6.uv.4 — Greenfield root: rwv creates pyproject.toml from scratch with
    /// `package=false`; deactivate fully removes it.
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.9 (uv greenfield)"]
    fn s6_uv_4_greenfield_create_then_deactivate_removes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "github/astral/protocol/pyproject.toml");

        // No root pyproject.toml.
        assert!(!root.join("pyproject.toml").exists());

        let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        UvWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(content.contains("github/astral/protocol"));
        // greenfield must set package = false so uv sync accepts the root.
        assert!(
            content.contains("package = false") || content.contains("package=false"),
            "greenfield root must declare [tool.uv].package = false; got:\n{content}"
        );

        // Marker must be present so deactivate can identify it.
        assert!(
            content.contains("managed by rwv"),
            "marker required for deactivate to act; got:\n{content}"
        );

        UvWorkspace.deactivate(root).unwrap();
        assert!(
            !root.join("pyproject.toml").exists(),
            "greenfield-created file must be deleted on deactivate"
        );
    }
}

// ===========================================================================
// cargo-workspace
// ===========================================================================

mod cargo_workspace {
    use super::*;

    #[test]
    fn auto_detects_repos_with_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");
        touch(root, "github/acme/web/Cargo.toml");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
            ("github/acme/docs", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_cargo_toml_with_workspace_section() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/Cargo.toml");
        touch(root, "github/chatly/server/Cargo.toml");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Owned),
            ("github/chatly/server", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.starts_with("# Generated by rwv \u{2014} do not edit\n"));
        assert!(content.contains("[workspace]"));
        assert!(content.contains("\"github/chatly/protocol\""));
        assert!(content.contains("\"github/chatly/server\""));
        assert!(content.contains("resolver = \"2\""));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");
        touch(root, "github/acme/reference-lib/Cargo.toml");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(!content.contains("reference-lib"));
    }

    #[test]
    fn deactivation_removes_generated_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Only removes if it starts with the generated-file header
        write_file(
            root,
            "Cargo.toml",
            "# Generated by rwv \u{2014} do not edit\n\n[workspace]\nmembers = []\nresolver = \"2\"\n",
        );
        assert!(root.join("Cargo.toml").exists());

        let integration = CargoWorkspace;
        integration.deactivate(root).unwrap();
        assert!(!root.join("Cargo.toml").exists());
    }

    #[test]
    fn deactivation_preserves_handwritten_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // A hand-written Cargo.toml without the generated header should NOT be removed
        write_file(
            root,
            "Cargo.toml",
            "[package]\nname = \"my-project\"\nversion = \"0.1.0\"\n",
        );
        assert!(root.join("Cargo.toml").exists());

        let integration = CargoWorkspace;
        integration.deactivate(root).unwrap();
        assert!(root.join("Cargo.toml").exists());
    }

    #[test]
    fn check_warns_when_cargo_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        let issues = integration.check(&ctx).unwrap();
        if which::which("cargo").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("cargo")));
        } else {
            assert!(issues.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Nested-workspace handling
    // -----------------------------------------------------------------------

    #[test]
    fn nested_workspace_without_opt_out_fails_with_named_repo_error() {
        // A member repo declares its own [workspace]. Activation must fail
        // before any cargo invocation, naming the conflicting repo and
        // listing the three resolutions.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/cwalv/plain/Cargo.toml",
            "[package]\nname = \"plain\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/cwalv/forked/Cargo.toml",
            "[package]\nname = \"forked\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [workspace]\nmembers = [\"crates/*\"]\n",
        );

        let manifest = make_manifest(vec![
            ("github/cwalv/plain", Role::Owned),
            ("github/cwalv/forked", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let err = CargoWorkspace.activate(&ctx).unwrap_err().to_string();
        assert!(
            err.contains("github/cwalv/forked"),
            "error should name the conflicting repo, got: {err}"
        );
        assert!(
            err.contains("[workspace]"),
            "error should explain the cause, got: {err}"
        );
        assert!(
            err.contains("exclude"),
            "error should point at the opt-out key, got: {err}"
        );
        // Generated Cargo.toml must NOT have been written on the failure path.
        assert!(
            !root.join("Cargo.toml").exists(),
            "no Cargo.toml should be written when activation fails"
        );
    }

    #[test]
    fn nested_workspace_with_opt_out_succeeds_and_emits_excluded_comment() {
        // With the opt-out set, the conflicting repo is dropped from members
        // and surfaced as a `# excluded:` comment in the generated file.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/cwalv/plain/Cargo.toml",
            "[package]\nname = \"plain\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/cwalv/forked/Cargo.toml",
            "[package]\nname = \"forked\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [workspace]\nmembers = [\"crates/*\"]\n",
        );

        let manifest = make_manifest(vec![
            ("github/cwalv/plain", Role::Owned),
            ("github/cwalv/forked", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("exclude: [github/cwalv/forked]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("# excluded: github/cwalv/forked (opted out)"),
            "missing excluded comment, got:\n{content}"
        );
        assert!(
            content.contains("\"github/cwalv/plain\""),
            "non-conflicting repo should still be a member, got:\n{content}"
        );
        assert!(
            !content.contains("\"github/cwalv/forked\""),
            "opted-out repo should not appear as a member, got:\n{content}"
        );
    }

    #[test]
    fn virtual_workspace_without_opt_out_fails_with_named_repo_error() {
        // Virtual workspace = `[workspace]` and no `[package]`. Same failure
        // mode as a hybrid workspace+package file.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/cwalv/plain/Cargo.toml",
            "[package]\nname = \"plain\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/cwalv/virtual_ws/Cargo.toml",
            "[workspace]\nmembers = [\"crate-a\", \"crate-b\"]\nresolver = \"2\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/cwalv/plain", Role::Owned),
            ("github/cwalv/virtual_ws", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let err = CargoWorkspace.activate(&ctx).unwrap_err().to_string();
        assert!(
            err.contains("github/cwalv/virtual_ws"),
            "virtual-workspace conflict should name the repo, got: {err}"
        );
    }

    #[test]
    fn opt_out_for_non_rust_repo_is_silently_ignored() {
        // Operators should be able to pre-emptively opt out a repo path even
        // if that repo isn't a Rust repo today; the integration should not
        // complain. (No-op fallback per bead spec.)
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/cwalv/plain/Cargo.toml",
            "[package]\nname = \"plain\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/cwalv/plain", Role::Owned),
            ("github/cwalv/docs-only", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config =
            IntegrationConfig::from_yaml("exclude: [github/cwalv/docs-only, github/missing/repo]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // No panic, no error — non-Rust opt-out entries are dropped.
        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // The non-Rust opt-out doesn't appear as a `# excluded:` comment
        // because the repo never made it through Cargo.toml detection.
        assert!(!content.contains("# excluded: github/cwalv/docs-only"));
        assert!(!content.contains("# excluded: github/missing/repo"));
        assert!(content.contains("\"github/cwalv/plain\""));
    }

    #[test]
    fn check_reports_nested_workspace_conflict_as_error_issue() {
        // `rwv doctor` should surface the same diagnostic as activation
        // would, so operators see it without having to attempt a lock.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/cwalv/forked/Cargo.toml",
            "[workspace]\nmembers = [\"crate-a\"]\n",
        );

        let manifest = make_manifest(vec![("github/cwalv/forked", Role::Fork)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = CargoWorkspace.check(&ctx).unwrap();
        assert!(
            issues.iter().any(|i| i.severity == Severity::Error
                && i.message.contains("github/cwalv/forked")),
            "check should report a nested-workspace error, got: {issues:?}"
        );
    }

    // -----------------------------------------------------------------------
    // §6 cargo-workspace — RED scenarios (fo-cnpjy.14 → green by C6+C7+C8)
    // -----------------------------------------------------------------------
    //
    // These mirror plan §6 cargo scenarios 1–4. Seeds use real idioms from
    // /home/cwa/weaveroot/rvtty/Cargo.toml (NOTE block, profile.* panic=abort,
    // workspace.lints.clippy) and astral-sh/ruff (workspace.dependencies,
    // workspace.package, profile.release.package.<crate>, workspace.lints.rust).
    // Members sub-path config (scenario 4) lands behind a config key C6/C8 ship.
    //
    // RED until C7 (cargo merge port) lands.

    /// §6.cargo.1 — Activate preserves rvtty's `[profile.*]` + `[workspace.lints]`.
    /// Seed file: verbatim from rvtty/Cargo.toml plus an empty members array
    /// and resolver. After activate, the NOTE comment block, panic="abort",
    /// and clippy deny policy must all survive byte-stable.
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.7 (cargo merge port)"]
    fn s6_1_activate_preserves_rvtty_profiles_and_lints() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Real Rust repos that the integration will detect as members.
        touch(root, "github/acme/rvtty-a/Cargo.toml");
        touch(root, "github/acme/rvtty-b/Cargo.toml");

        // Seed the root Cargo.toml with rvtty's idioms (NOTE block, profile
        // panic=abort, workspace.lints.clippy).
        write_file(
            root,
            "Cargo.toml",
            r#"# Generated by rwv — do not edit
#
# NOTE (olb.5.4): rvtty-style hand-maintained block. profile/lint policy
# must round-trip activate untouched. This comment block is part of the
# regression — strip it and the rationale is lost forever.

[workspace]
members = []
resolver = "2"

# Panic strategy: abort in all profiles.
[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"

# Workspace-wide clippy lint policy.
[workspace.lints.clippy]
print_stdout = "deny"
print_stderr = "deny"
dbg_macro    = "deny"
"#,
        );

        let manifest = make_manifest(vec![
            ("github/acme/rvtty-a", Role::Owned),
            ("github/acme/rvtty-b", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let path = root.join("Cargo.toml");
        contract::assert_activate_preserves_foreign(
            &path,
            || {
                CargoWorkspace.activate(&ctx).unwrap();
            },
            &[
                contract::substr_probe("members[rvtty-a]", "github/acme/rvtty-a"),
                contract::substr_probe("members[rvtty-b]", "github/acme/rvtty-b"),
                contract::substr_probe("resolver", "resolver = \"2\""),
            ],
            // Marker: per-key `# managed by rwv` decor on `members` (per plan
            // §5a; TomlDoc impl). C7 must set this on author.
            &contract::substr_probe("toml marker on members", "managed by rwv"),
            &[
                "NOTE (olb.5.4)",
                "panic = \"abort\"",
                "print_stdout = \"deny\"",
                "print_stderr = \"deny\"",
                "dbg_macro",
                "# Panic strategy",
                "[workspace.lints.clippy]",
            ],
        );
    }

    /// §6.cargo.2 — Re-activate is idempotent w.r.t. `[workspace.dependencies]`
    /// / `[workspace.package]` / `[profile.*]` (the ruff surface).
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.7 (cargo merge port)"]
    fn s6_2_reactivate_idempotent_over_ruff_surface() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/astral/ruff/Cargo.toml");
        touch(root, "github/astral/ty/Cargo.toml");

        // Ruff-idiom hand-maintained root with workspace.package, deps, lints,
        // and a profile.release.package.<crate> entry.
        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
members = ["github/astral/ruff"]
resolver = "2"

[workspace.package]
edition = "2021"
rust-version = "1.78"
homepage = "https://docs.astral.sh/ruff"
license = "MIT"

[workspace.dependencies]
anyhow = "1.0.80"
serde = { version = "1.0", features = ["derive"] }

[workspace.lints.rust]
unsafe_code = "warn"

[profile.release]
lto = "fat"
codegen-units = 1

[profile.release.package.ruff_python_parser]
codegen-units = 1
"#,
        );

        let manifest = make_manifest(vec![
            ("github/astral/ruff", Role::Owned),
            ("github/astral/ty", Role::Owned),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let path = root.join("Cargo.toml");
        contract::assert_activate_idempotent(
            &path,
            || {
                CargoWorkspace.activate(&ctx).unwrap();
            },
            |_p| { /* no user mutation between activates */ },
            &[
                contract::substr_probe("members[ruff]", "github/astral/ruff"),
                contract::substr_probe("members[ty]", "github/astral/ty"),
            ],
            &contract::substr_probe("toml marker on members", "managed by rwv"),
            &[
                "[workspace.package]",
                "rust-version = \"1.78\"",
                "[workspace.dependencies]",
                "anyhow = \"1.0.80\"",
                "[workspace.lints.rust]",
                "unsafe_code = \"warn\"",
                "[profile.release]",
                "lto = \"fat\"",
                "[profile.release.package.ruff_python_parser]",
            ],
        );
    }

    /// §6.cargo.3 — Deactivate strips only rwv keys, keeps user policy.
    /// Regression-proof against current delete-whole at cargo_workspace.rs:182-184.
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.7 (cargo merge port)"]
    fn s6_3_deactivate_strips_keeps_user_policy() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Seed with marker + owned keys + heavy user policy.
        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
# managed by rwv
members = ["github/acme/server"]
# managed by rwv
resolver = "2"

[profile.dev]
panic = "abort"

[workspace.lints.clippy]
dbg_macro = "deny"

[patch.crates-io]
foo = { path = "vendor/foo" }
"#,
        );

        let path = root.join("Cargo.toml");
        contract::assert_deactivate_strips_keeps(
            &path,
            || {
                CargoWorkspace.deactivate(root).unwrap();
            },
            &[
                contract::substr_probe("members entry", "github/acme/server"),
                contract::substr_probe("resolver", "resolver = \"2\""),
            ],
            &contract::substr_probe("toml marker", "managed by rwv"),
            &[
                "[profile.dev]",
                "panic = \"abort\"",
                "[workspace.lints.clippy]",
                "dbg_macro = \"deny\"",
                "[patch.crates-io]",
                "foo = { path = \"vendor/foo\" }",
            ],
        );
    }

    /// §6.cargo.4 — Members sub-path config + nested-workspace exemption
    /// (rvtty end-state). Repo with no root Cargo.toml; config emits
    /// `<repo>/<sub>` per include. Sibling workspace is NOT an ancestor and
    /// must NOT trip the nested-workspace error.
    ///
    /// Members sub-path config is added by C6 (cargo design-finalization) +
    /// C8 (cargo members-subpath + [patch] opt-in).
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.8 (members sub-path)"]
    fn s6_4_members_subpath_and_nested_workspace_exemption() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No root Cargo.toml under github/cwalv/rvtty.
        write_file(
            root,
            "github/cwalv/rvtty/daemon/Cargo.toml",
            "[package]\nname = \"daemon\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/cwalv/rvtty/client/Cargo.toml",
            "[package]\nname = \"client\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/cwalv/rvtty/common/Cargo.toml",
            "[package]\nname = \"common\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        // Sibling workspace (not an ancestor).
        write_file(
            root,
            "rvtty/workspace/Cargo.toml",
            "[workspace]\nmembers = [\"../daemon\", \"../client\"]\nresolver = \"2\"\n",
        );

        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project");
        // The members-subpath config shape (C6/C8): per-repo sub-path include
        // list. Exact YAML key path locked in by C6; this is the plan §5a shape.
        let config = IntegrationConfig::from_yaml(
            "members:\n  github/cwalv/rvtty:\n    include: [daemon, client, common]\n",
        );
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Must NOT fail with nested-workspace error: the sibling workspace at
        // rvtty/workspace/Cargo.toml is not an ancestor of any included sub-path.
        CargoWorkspace.activate(&ctx).expect(
            "activate must succeed: members.<repo> exempts the root, and the \
             sibling workspace is not an ancestor of the included sub-paths",
        );

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("github/cwalv/rvtty/daemon"),
            "members should include daemon sub-path; got:\n{content}"
        );
        assert!(
            content.contains("github/cwalv/rvtty/client"),
            "members should include client sub-path; got:\n{content}"
        );
        assert!(
            content.contains("github/cwalv/rvtty/common"),
            "members should include common sub-path; got:\n{content}"
        );
        // The repo root itself must NOT be a member (no root Cargo.toml).
        let lines_with_repo_root: Vec<&str> = content
            .lines()
            .filter(|l| l.contains("\"github/cwalv/rvtty\""))
            .collect();
        assert!(
            lines_with_repo_root.is_empty(),
            "repo root (github/cwalv/rvtty) must NOT appear as a member; got:\n{content}"
        );
    }
}

// ===========================================================================
// gita
// ===========================================================================

mod gita {
    use super::*;

    #[test]
    fn auto_detects_all_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // gita uses all repos, not just those with a specific manifest file
        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        integration.activate(&ctx).unwrap();

        let repos_csv = std::fs::read_to_string(root.join("gita/repos.csv")).unwrap();
        assert!(repos_csv.contains("server"));
        assert!(repos_csv.contains("web"));
    }

    #[test]
    fn generates_repos_csv_with_correct_format() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Owned),
            ("github/chatly/web", Role::Owned),
            ("github/chatly/protocol", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        integration.activate(&ctx).unwrap();

        let repos_csv = std::fs::read_to_string(root.join("gita/repos.csv")).unwrap();
        assert!(repos_csv.starts_with("path,name,flags\n"));

        let lines: Vec<&str> = repos_csv.lines().collect();
        // Header + 3 repos
        assert_eq!(lines.len(), 4);

        // Should be sorted by name (basename)
        assert!(lines[1].contains(",protocol,"));
        assert!(lines[2].contains(",server,"));
        assert!(lines[3].contains(",web,"));

        // Paths should be absolute
        let abs_prefix = root.to_string_lossy();
        assert!(lines[1].starts_with(&*abs_prefix));
    }

    #[test]
    fn generates_groups_csv_by_role() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Owned),
            ("github/chatly/web", Role::Owned),
            ("github/chatly/protocol", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        integration.activate(&ctx).unwrap();

        let groups_csv = std::fs::read_to_string(root.join("gita/groups.csv")).unwrap();
        assert!(groups_csv.starts_with("group,repos\n"));
        assert!(groups_csv.contains("fork,protocol\n"));
        assert!(groups_csv.contains("owned,server web\n"));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        integration.activate(&ctx).unwrap();

        let repos_csv = std::fs::read_to_string(root.join("gita/repos.csv")).unwrap();
        assert!(repos_csv.contains("server"));
        assert!(!repos_csv.contains("reference-lib"));

        let groups_csv = std::fs::read_to_string(root.join("gita/groups.csv")).unwrap();
        assert!(!groups_csv.contains("reference"));
    }

    #[test]
    fn deactivation_removes_gita_directory() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("gita")).unwrap();
        write_file(root, "gita/repos.csv", "path,name,flags\n");
        write_file(root, "gita/groups.csv", "group,repos\n");
        assert!(root.join("gita").exists());

        let integration = Gita;
        integration.deactivate(root).unwrap();
        assert!(!root.join("gita").exists());
    }

    #[test]
    fn repos_csv_paths_use_workspace_root_not_output_dir() {
        let workspace_tmp = TempDir::new().unwrap();
        let workspace_root = workspace_tmp.path();
        let weave_tmp = TempDir::new().unwrap();
        let output_dir = weave_tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir,
            workspace_root,
            project: &project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config: &config,
            all_repos_on_disk: &[],
            all_project_paths: &[],
            detection_cache: &cache,
            workweave: None,
        };

        let integration = Gita;
        integration.activate(&ctx).unwrap();

        let repos_csv = std::fs::read_to_string(output_dir.join("gita/repos.csv")).unwrap();
        let ws_prefix = workspace_root.to_string_lossy();
        let out_prefix = output_dir.to_string_lossy();
        // Repo paths must point to workspace_root (where repos live), not output_dir
        assert!(
            repos_csv.contains(&*ws_prefix),
            "repos.csv should contain workspace_root path: {}",
            repos_csv
        );
        // output_dir and workspace_root are different TempDirs, so output_dir
        // should NOT appear in the path column.
        let data_lines: Vec<&str> = repos_csv.lines().skip(1).collect();
        for line in &data_lines {
            let path_field = line.split(',').next().unwrap();
            assert!(
                !path_field.starts_with(&*out_prefix),
                "repo path should not start with output_dir: {}",
                line
            );
        }
    }

    #[test]
    fn check_warns_when_gita_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        let issues = integration.check(&ctx).unwrap();
        if which::which("gita").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("gita")));
        } else {
            assert!(issues.is_empty());
        }
    }

    /// A repo path containing a comma must be emitted as a properly-quoted CSV
    /// field and must round-trip through csv::Reader without corruption.
    /// Pre-fix, the concat-based writer produced a malformed row.
    #[test]
    fn csv_escaping_roundtrips_path_with_comma() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Use a repo path whose basename contains a comma. The manifest helper
        // only sets the `url` from the last path segment, so we synthesise the
        // manifest YAML directly to include an unusual path key.
        let yaml = "repositories:\n  \"github/owner/with,comma\":\n    type: git\n    url: https://github.com/owner/withcomma.git\n    version: main\n    role: owned\n";
        let manifest = repoweave::manifest::Manifest::from_yaml_str(yaml).unwrap();
        let project = repoweave::manifest::ProjectName::new("test-project");
        let config = repoweave::manifest::IntegrationConfig::default();
        let cache = std::collections::HashMap::new();
        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config: &config,
            all_repos_on_disk: &[],
            all_project_paths: &[],
            detection_cache: &cache,
            workweave: None,
        };

        Gita.activate(&ctx).unwrap();

        // Round-trip: parse with csv::Reader and verify the path field is intact.
        let repos_csv_path = root.join("gita/repos.csv");
        let mut rdr = csv::Reader::from_path(&repos_csv_path).unwrap();
        let records: Vec<_> = rdr.records().collect::<Result<_, _>>().unwrap();
        assert_eq!(records.len(), 1, "expected exactly one data row");
        let path_field = &records[0][0];
        // The path column should end with the repo path including the comma.
        assert!(
            path_field.ends_with("github/owner/with,comma"),
            "path field should contain the comma-repo path, got: {path_field:?}"
        );
        // And the name field (basename) should be correct too.
        let name_field = &records[0][1];
        assert_eq!(
            name_field, "with,comma",
            "name field should be the basename, got: {name_field:?}"
        );
    }

    /// Deactivate must remove the two rwv-owned CSVs but leave a user-parked
    /// file (e.g. notes.txt) and the gita/ directory intact.
    #[test]
    fn deactivate_preserves_user_parked_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("gita")).unwrap();
        write_file(root, "gita/repos.csv", "path,name,flags\n");
        write_file(root, "gita/groups.csv", "group,repos\n");
        write_file(root, "gita/notes.txt", "keep me\n");

        Gita.deactivate(root).unwrap();

        assert!(
            !root.join("gita/repos.csv").exists(),
            "repos.csv should be removed"
        );
        assert!(
            !root.join("gita/groups.csv").exists(),
            "groups.csv should be removed"
        );
        assert!(
            root.join("gita/notes.txt").exists(),
            "user-parked notes.txt should survive deactivate"
        );
        assert!(
            root.join("gita").exists(),
            "gita/ directory should survive when non-empty"
        );
    }

    /// Deactivate must remove the gita/ directory entirely when it becomes
    /// empty after removing the two rwv-owned CSVs.
    #[test]
    fn deactivate_removes_empty_gita_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("gita")).unwrap();
        write_file(root, "gita/repos.csv", "path,name,flags\n");
        write_file(root, "gita/groups.csv", "group,repos\n");

        Gita.deactivate(root).unwrap();

        assert!(
            !root.join("gita/repos.csv").exists(),
            "repos.csv should be removed"
        );
        assert!(
            !root.join("gita/groups.csv").exists(),
            "groups.csv should be removed"
        );
        assert!(
            !root.join("gita").exists(),
            "gita/ directory should be removed when empty"
        );
    }
}

// ===========================================================================
// vscode-workspace
// ===========================================================================

mod vscode_workspace {
    use super::*;

    #[test]
    fn auto_detects_all_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // vscode-workspace uses all repos (not filtered by manifest file)
        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        integration.activate(&ctx).unwrap();
        assert!(root.join("test-project.code-workspace").exists());
    }

    #[test]
    fn generates_code_workspace_file_with_folders_and_settings() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Owned),
            ("github/chatly/web", Role::Owned),
        ]);
        let project = ProjectName::new("web-app");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("web-app.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        let folders = parsed["folders"].as_array().unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0]["path"], ".");
        assert_eq!(folders[0]["name"], "web-app (primary)");

        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"],
            "subFolders"
        );
        assert_eq!(parsed["settings"]["git.repositoryScanMaxDepth"], 3);

        // Should include the generated marker so deactivate can identify it.
        // The marker is now an object { "managed": true, "files.exclude": [...] }
        // (was plain `true` before fo-cnpjy.5 — has_marker tolerates both forms).
        assert_eq!(parsed["rwv.generated"]["managed"], true);
    }

    #[test]
    fn project_name_appears_in_filename() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("my-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        integration.activate(&ctx).unwrap();
        assert!(root.join("my-project.code-workspace").exists());
    }

    #[test]
    fn preserves_user_customizations_on_reactivation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Pre-existing workspace file with user customizations
        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "folders": [{ "path": ".", "name": "old-name" }],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "editor.fontSize": 14
  },
  "extensions": {
    "recommendations": ["rust-analyzer"]
  }
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Folders should be updated
        let folders = parsed["folders"].as_array().unwrap();
        assert_eq!(folders[0]["name"], "test-project (primary)");

        // Managed settings should be updated
        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"],
            "subFolders"
        );
        assert_eq!(parsed["settings"]["git.repositoryScanMaxDepth"], 3);

        // User customizations should survive
        assert_eq!(parsed["settings"]["editor.fontSize"], 14);
        assert!(parsed["extensions"]["recommendations"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("rust-analyzer")));
    }

    #[test]
    fn deactivation_removes_generated_code_workspace_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Write a file with the rwv.generated marker (as activate produces).
        write_file(
            root,
            "test-project.code-workspace",
            r#"{"rwv.generated": true, "folders": []}"#,
        );
        assert!(root.join("test-project.code-workspace").exists());

        let integration = VscodeWorkspace;
        integration.deactivate(root).unwrap();
        assert!(!root.join("test-project.code-workspace").exists());
    }

    #[test]
    fn deactivation_preserves_handwritten_code_workspace_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // A user-created .code-workspace without the rwv marker.
        write_file(
            root,
            "my-project.code-workspace",
            r#"{"folders": [{"path": "."}]}"#,
        );

        let integration = VscodeWorkspace;
        integration.deactivate(root).unwrap();
        assert!(
            root.join("my-project.code-workspace").exists(),
            "hand-written .code-workspace should be preserved"
        );
    }

    #[test]
    fn check_validates_workspace_file_exists() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No .code-workspace file present
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        let issues = integration.check(&ctx).unwrap();
        assert!(issues
            .iter()
            .any(|i| i.severity == Severity::Warning && i.message.contains("code-workspace")));
    }

    #[test]
    fn files_exclude_hides_non_project_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Active project has github/chatly/server.
        // github/acme/web is on disk but not in the project.
        let manifest = make_manifest(vec![("github/chatly/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();

        let all_repos_on_disk: Vec<RepoPath> = vec![
            RepoPath::new("github/chatly/server").expect("known-safe literal"),
            RepoPath::new("github/acme/web").expect("known-safe literal"),
        ];

        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config: &config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &[],
            detection_cache: &cache,
            workweave: None,
        };

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        let exclude = &parsed["settings"]["files.exclude"];
        // github/acme/web should be excluded (only repo under github/acme, so
        // collapse_excludes will produce "github/acme")
        assert_eq!(exclude["github/acme"], serde_json::Value::Bool(true));
        // github/chatly/server is active — must NOT be excluded
        assert!(exclude.get("github/chatly/server").is_none());
        assert!(exclude.get("github/chatly").is_none());
        assert!(exclude.get("github").is_none());
    }

    #[test]
    fn files_exclude_hides_other_projects() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("proj-a");
        let config = IntegrationConfig::default();

        let all_repos_on_disk: Vec<RepoPath> =
            vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
        let all_project_paths = vec!["proj-a".to_string(), "proj-b".to_string()];

        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config: &config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &all_project_paths,
            detection_cache: &HashMap::new(),
            workweave: None,
        };

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("proj-a.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let exclude = &parsed["settings"]["files.exclude"];

        // The other project directory should be excluded.
        assert_eq!(exclude["projects/proj-b"], serde_json::Value::Bool(true));
        // The active project should NOT be excluded.
        assert!(exclude.get("projects/proj-a").is_none());
    }

    #[test]
    fn files_exclude_hides_dotfiles_by_default() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            parsed["settings"]["files.exclude"][".*"],
            serde_json::Value::Bool(true),
            "dotfiles should be hidden by default"
        );
    }

    #[test]
    fn files_exclude_respects_hide_dotfiles_false() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("hide-dotfiles: false");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(
            parsed["settings"]["files.exclude"].get(".*").is_none(),
            "dotfiles should not be hidden when hide-dotfiles is false"
        );
    }

    #[test]
    fn files_exclude_collapses_paths() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Active project has only github/acme/server.
        // All other repos are under github/other — should collapse to github/other.
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();

        let all_repos_on_disk: Vec<RepoPath> = vec![
            RepoPath::new("github/acme/server").expect("known-safe literal"),
            RepoPath::new("github/other/alpha").expect("known-safe literal"),
            RepoPath::new("github/other/beta").expect("known-safe literal"),
        ];

        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config: &config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &[],
            detection_cache: &cache,
            workweave: None,
        };

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let exclude = &parsed["settings"]["files.exclude"];

        // All repos under github/other excluded → should collapse to owner path.
        assert_eq!(exclude["github/other"], serde_json::Value::Bool(true));
        // Individual paths should NOT appear (they were collapsed).
        assert!(exclude.get("github/other/alpha").is_none());
        assert!(exclude.get("github/other/beta").is_none());
        // Active repo and its owner must not be excluded.
        assert!(exclude.get("github/acme").is_none());
        assert!(exclude.get("github/acme/server").is_none());
    }

    // -----------------------------------------------------------------------
    // §6 vscode-workspace — RED scenarios (fo-cnpjy.14 → green by C5)
    // -----------------------------------------------------------------------
    //
    // RED against current `:178-181` (per-key files.exclude merge), `:119-122`
    // (multi-root folders preservation), and `:209` (strip-not-delete deactivate).

    /// §6.vscode.1 — User adds a personal `files.exclude` entry; sync must
    /// not eat it. RED vs current :178-181 (whole-map insert).
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.5 (vscode per-key files.exclude)"]
    fn s6_vscode_1_user_files_exclude_entries_survive_activate() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Pre-existing rwv-generated workspace with rwv-owned files.exclude
        // entries (.* + projects/foundations-test) + user-added entries
        // (**/target and dist).
        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {
      ".*": true,
      "projects/foundations-test": true,
      "**/target": true,
      "dist": true
    }
  }
}"#,
        );

        // New repo on disk — a fresh activation cycle.
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let all_repos_on_disk: Vec<RepoPath> =
            vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config: &config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &[],
            detection_cache: &cache,
            workweave: None,
        };

        VscodeWorkspace.activate(&ctx).unwrap();
        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        let exclude = &parsed["settings"]["files.exclude"];
        // User-added keys MUST survive.
        assert_eq!(
            exclude["**/target"],
            serde_json::Value::Bool(true),
            "user-added **/target must survive activate; got: {exclude}"
        );
        assert_eq!(
            exclude["dist"],
            serde_json::Value::Bool(true),
            "user-added dist must survive activate; got: {exclude}"
        );
        // rwv-owned keys still set correctly.
        assert_eq!(exclude[".*"], serde_json::Value::Bool(true));
        assert_eq!(parsed["rwv.generated"], true);
    }

    /// §6.vscode.2 — User-added extensions/launch/tasks/compounds survive
    /// activate AND deactivate. RED vs current :209 (whole-file delete).
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.5 (vscode strip-not-delete)"]
    fn s6_vscode_2_user_top_level_blocks_survive_activate_and_deactivate() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3
  },
  "extensions": {
    "recommendations": ["rust-lang.rust-analyzer", "tamasfe.even-better-toml"]
  },
  "launch": {
    "configurations": [
      { "type": "lldb", "request": "launch", "name": "debug rvtty" }
    ]
  },
  "tasks": {
    "version": "2.0.0",
    "tasks": [{ "label": "build", "type": "shell", "command": "cargo build" }]
  },
  "compounds": [
    { "name": "all-services", "configurations": ["debug rvtty"] }
  ]
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // After activate: all four blocks must be intact.
        VscodeWorkspace.activate(&ctx).unwrap();
        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            parsed["extensions"]["recommendations"]
                .as_array()
                .map(|a| a.iter().any(|v| v.as_str() == Some("rust-lang.rust-analyzer")))
                .unwrap_or(false),
            "extensions.recommendations must survive activate; got: {parsed}"
        );
        assert!(
            parsed["launch"]["configurations"][0]["name"].as_str() == Some("debug rvtty"),
            "launch.configurations must survive activate; got: {parsed}"
        );
        assert_eq!(parsed["tasks"]["version"], "2.0.0", "tasks must survive");
        assert_eq!(
            parsed["compounds"][0]["name"], "all-services",
            "compounds must survive"
        );

        // After deactivate: file MUST persist (not be deleted); owned keys
        // stripped (folders, settings.git.*, settings.files.exclude, marker);
        // the four user blocks remain.
        VscodeWorkspace.deactivate(root).unwrap();
        assert!(
            root.join("test-project.code-workspace").exists(),
            "deactivate must NOT delete a file with user-authored blocks"
        );
        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            parsed.get("rwv.generated").is_none(),
            "marker must be stripped; got: {parsed}"
        );
        assert!(
            parsed.get("folders").is_none(),
            "folders is owned; must be stripped; got: {parsed}"
        );
        // User content MUST remain.
        assert!(
            parsed["extensions"]["recommendations"].is_array(),
            "extensions must survive deactivate"
        );
        assert!(parsed["launch"]["configurations"].is_array());
        assert!(parsed["tasks"]["tasks"].is_array());
        assert!(parsed["compounds"].is_array());
    }

    /// §6.vscode.3 — User converts to multi-root; rwv keeps the extra folder.
    /// RED vs current :119-122 (whole-array overwrite).
    #[test]
    #[ignore = "RED: turned green by fo-cnpjy.5 (vscode multi-root folders)"]
    fn s6_vscode_3_user_added_folder_survives_multi_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [
    { "path": ".", "name": "test-project (primary)" },
    { "path": "../shared-notes", "name": "notes" }
  ]
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();
        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let folders = parsed["folders"].as_array().unwrap();

        assert!(folders.len() >= 2, "folders must include both entries; got: {folders:?}");
        // Primary folder is rwv-owned and refreshed.
        assert!(
            folders.iter().any(|f| f["path"].as_str() == Some(".")),
            "primary `.` folder must be present; got: {folders:?}"
        );
        // User-added notes folder MUST survive (dedupe on path).
        assert!(
            folders.iter().any(|f| {
                f["path"].as_str() == Some("../shared-notes") && f["name"].as_str() == Some("notes")
            }),
            "user-added folder must survive; got: {folders:?}"
        );
        assert_eq!(parsed["rwv.generated"], true);
    }

    /// §6.vscode.4 — Deactivate of a purely-rwv file deletes it; hand-written
    /// file (no marker) is untouched.
    ///
    /// Currently GREEN — the current vscode deactivate already gates on the
    /// rwv.generated marker and deletes the whole file (which happens to
    /// satisfy this scenario despite being the bug elsewhere). Keep ungated
    /// as a regression guard against C5 (when C5 switches to strip-not-delete,
    /// this scenario must still pass because the post-strip doc is empty).
    #[test]
    fn s6_vscode_4_deactivate_deletes_purely_rwv_preserves_hand_written() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // (a) Purely-rwv-owned file: marker + only owned content.
        write_file(
            root,
            "proj.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [{ "path": ".", "name": "proj (primary)" }],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": { ".*": true }
  }
}"#,
        );

        // (b) Hand-written file: NO marker, real user content.
        write_file(
            root,
            "mine.code-workspace",
            r#"{
  "folders": [{ "path": "." }],
  "settings": { "editor.tabSize": 2 }
}"#,
        );

        VscodeWorkspace.deactivate(root).unwrap();

        assert!(
            !root.join("proj.code-workspace").exists(),
            "purely-rwv file must be deleted (residual empty)"
        );
        assert!(
            root.join("mine.code-workspace").exists(),
            "hand-written .code-workspace (no marker) must be preserved"
        );
        let mine = std::fs::read_to_string(root.join("mine.code-workspace")).unwrap();
        assert!(
            mine.contains("editor.tabSize"),
            "hand-written file content must be untouched; got: {mine}"
        );
    }
}

// ===========================================================================
// vscode-workspace: §6 residual-bug scenarios (fo-cnpjy.5)
// ===========================================================================
//
// Scenarios 1–4 from plan §6 "vscode-workspace". Each scenario pins one of
// the four residual bugs fixed by fo-cnpjy.5. They were RED against the
// pre-fix code; they are GREEN after the fixes land.

mod vscode_workspace_scenarios {
    use super::*;

    // -------------------------------------------------------------------------
    // Scenario 1 — User adds a personal `files.exclude` entry; sync must not
    // eat it.
    //
    // Plan §6 vscode scenario 1: "User adds personal `files.exclude` entry;
    // sync preserves it. RED today against :178-181."
    // -------------------------------------------------------------------------
    #[test]
    fn scenario1_user_files_exclude_survives_reactivation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Seed: an rwv-generated workspace file (marker present, primary folder,
        // git.* settings, rwv-derived exclude keys) PLUS two user-added exclude
        // entries that rwv should never touch.
        write_file(
            root,
            "foundations.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [{"path": ".", "name": "foundations (primary)"}],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {
      ".*": true,
      "github/acme": true,
      "**/target": true,
      "dist": true
    }
  }
}"#,
        );

        // Activate again with a new repo on disk (github/chatly/api joins).
        // github/acme is still excluded (not in manifest).
        let manifest = make_manifest(vec![
            ("github/cwalv/repoweave", Role::Owned),
            ("github/chatly/api", Role::Owned),
        ]);
        let project = ProjectName::new("foundations");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();

        let all_repos_on_disk: Vec<RepoPath> = vec![
            RepoPath::new("github/cwalv/repoweave").expect("known-safe literal"),
            RepoPath::new("github/chatly/api").expect("known-safe literal"),
            RepoPath::new("github/acme/server").expect("known-safe literal"),
            RepoPath::new("github/acme/web").expect("known-safe literal"),
        ];

        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config: &config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &[],
            detection_cache: &cache,
            workweave: None,
        };

        VscodeWorkspace.activate(&ctx).unwrap();

        let content =
            std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let exclude = &parsed["settings"]["files.exclude"];

        // User-added keys MUST survive (this was the bug: they were wiped).
        assert_eq!(
            exclude["**/target"],
            serde_json::Value::Bool(true),
            "user-added **/target must survive reactivation"
        );
        assert_eq!(
            exclude["dist"],
            serde_json::Value::Bool(true),
            "user-added dist must survive reactivation"
        );

        // rwv-derived keys should be correct for the new state.
        // github/acme is still excluded (both repos excluded → collapses to owner).
        assert_eq!(
            exclude["github/acme"],
            serde_json::Value::Bool(true),
            "rwv-derived exclude for github/acme must be present"
        );

        // The marker and git.* keys must still be present.
        assert_eq!(parsed["rwv.generated"]["managed"], serde_json::Value::Bool(true));
        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"],
            "subFolders"
        );
    }

    // -------------------------------------------------------------------------
    // Scenario 2 — User adds `extensions`/`launch`/`tasks`/`compounds`; they
    // survive activate AND deactivate.
    //
    // Plan §6 vscode scenario 2: "User adds extensions/launch/tasks/compounds;
    // survive activate AND deactivate. RED today against :209."
    // -------------------------------------------------------------------------
    #[test]
    fn scenario2_user_blocks_survive_activate_and_deactivate() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Seed: rwv-generated workspace + the four user-added top-level blocks.
        write_file(
            root,
            "myproject.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [{"path": ".", "name": "myproject (primary)"}],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {".*": true}
  },
  "extensions": {
    "recommendations": ["rust-analyzer", "vadimcn.vscode-lldb"]
  },
  "launch": {
    "version": "0.2.0",
    "configurations": [{"type": "lldb", "request": "launch", "name": "Debug"}]
  },
  "tasks": {
    "version": "2.0.0",
    "tasks": [{"label": "build", "type": "shell", "command": "cargo build"}]
  },
  "compounds": [{"name": "Full debug", "configurations": ["Debug"]}]
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("myproject");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Activate: all four user blocks must survive.
        VscodeWorkspace.activate(&ctx).unwrap();

        let content =
            std::fs::read_to_string(root.join("myproject.code-workspace")).unwrap();
        let after_activate: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(
            after_activate["extensions"]["recommendations"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("rust-analyzer")),
            "extensions must survive activate"
        );
        assert!(
            after_activate["launch"]["version"].as_str() == Some("0.2.0"),
            "launch must survive activate"
        );
        assert!(
            after_activate["tasks"]["version"].as_str() == Some("2.0.0"),
            "tasks must survive activate"
        );
        assert!(
            after_activate["compounds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == "Full debug"),
            "compounds must survive activate"
        );

        // Deactivate: file must NOT be deleted; owned keys stripped but user
        // blocks survive.
        VscodeWorkspace.deactivate(root).unwrap();

        assert!(
            root.join("myproject.code-workspace").exists(),
            "file must NOT be deleted — user content remains"
        );

        let content =
            std::fs::read_to_string(root.join("myproject.code-workspace")).unwrap();
        let after_deactivate: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Owned keys stripped.
        assert!(
            after_deactivate.get("rwv.generated").is_none(),
            "marker must be stripped on deactivate"
        );
        assert!(
            after_deactivate.get("folders").is_none(),
            "folders must be stripped on deactivate"
        );
        assert!(
            after_deactivate["settings"]
                .as_object()
                .map(|m| m.get("files.exclude").is_none())
                .unwrap_or(true),
            "files.exclude must be stripped on deactivate"
        );

        // User blocks preserved.
        assert!(
            after_deactivate["extensions"]["recommendations"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("rust-analyzer")),
            "extensions must survive deactivate"
        );
        assert!(
            after_deactivate["launch"]["version"].as_str() == Some("0.2.0"),
            "launch must survive deactivate"
        );
        assert!(
            after_deactivate["tasks"]["version"].as_str() == Some("2.0.0"),
            "tasks must survive deactivate"
        );
        assert!(
            after_deactivate["compounds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == "Full debug"),
            "compounds must survive deactivate"
        );
    }

    // -------------------------------------------------------------------------
    // Scenario 3 — User converts to multi-root; rwv keeps the extra folder.
    //
    // Plan §6 vscode scenario 3: "User converts to multi-root; rwv keeps the
    // extra folder. RED today against :119-122."
    // -------------------------------------------------------------------------
    #[test]
    fn scenario3_user_extra_folder_survives_reactivation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Seed: primary folder + a user-added extra folder (shared-notes).
        write_file(
            root,
            "foundations.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [
    {"path": ".", "name": "foundations (primary)"},
    {"name": "notes", "path": "../shared-notes"}
  ],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {".*": true}
  }
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("foundations");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let content =
            std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let folders = parsed["folders"].as_array().unwrap();

        // BOTH folders must be present.
        assert_eq!(
            folders.len(),
            2,
            "both folders must be present after reactivation; got: {folders:?}"
        );

        // Element 0 must be the rwv-managed primary.
        assert_eq!(
            folders[0]["path"], ".",
            "primary folder must be at index 0"
        );
        assert_eq!(
            folders[0]["name"], "foundations (primary)",
            "primary folder name must be updated"
        );

        // Element 1 must be the user-added extra folder, preserved unchanged.
        assert_eq!(
            folders[1]["path"], "../shared-notes",
            "user-added folder path must survive"
        );
        assert_eq!(
            folders[1]["name"], "notes",
            "user-added folder name must survive"
        );

        // Marker still present (object form after fo-cnpjy.5).
        assert_eq!(parsed["rwv.generated"]["managed"], serde_json::Value::Bool(true));
    }

    // -------------------------------------------------------------------------
    // Scenario 4 — Deactivate of a purely-rwv file deletes it; hand-written
    // file (no marker) is untouched.
    //
    // Plan §6 vscode scenario 4: "Deactivate of a pure-rwv file deletes it;
    // hand-written file (no marker) untouched."
    // -------------------------------------------------------------------------
    #[test]
    fn scenario4_deactivate_deletes_pure_rwv_file_leaves_handwritten() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // (a) A purely-rwv .code-workspace: marker + owned keys only.
        write_file(
            root,
            "proj.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [{"path": ".", "name": "proj (primary)"}],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {".*": true}
  }
}"#,
        );

        // (b) A hand-written .code-workspace with no rwv marker.
        write_file(
            root,
            "mine.code-workspace",
            r#"{
  "folders": [{"path": "."}],
  "settings": {"editor.tabSize": 2}
}"#,
        );

        VscodeWorkspace.deactivate(root).unwrap();

        // (a) Purely-rwv file: all content was owned → delete it.
        assert!(
            !root.join("proj.code-workspace").exists(),
            "purely-rwv file must be deleted on deactivate"
        );

        // (b) Hand-written file: no marker → must not be touched.
        assert!(
            root.join("mine.code-workspace").exists(),
            "hand-written file must survive deactivate"
        );
        let content =
            std::fs::read_to_string(root.join("mine.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["settings"]["editor.tabSize"],
            serde_json::Value::Number(2.into()),
            "hand-written file content must be byte-identical"
        );
    }
}

// ===========================================================================
// Integration activate hooks
// ===========================================================================
//
// Each ecosystem integration should have an `activate_hook` that runs the
// install command. Non-ecosystem integrations (gita, vscode) should have
// no-op hooks.

mod activate_hooks {
    use super::*;

    // -----------------------------------------------------------------------
    // npm-workspaces: `npm install`
    // -----------------------------------------------------------------------

    #[test]
    fn npm_workspaces_activate_hook_runs_npm_install() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Set up a repo with a valid package.json so npm integration detects it
        write_file(
            root,
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}",
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Activate first so the root package.json exists
        let integration = NpmWorkspaces;
        integration.activate(&ctx).unwrap();

        // Activate hook should succeed (runs `npm install`)
        let result = integration.activate_hook(&ctx);
        if which::which("npm").is_ok() {
            assert!(
                result.is_ok(),
                "npm activate hook should succeed when npm is available: {:?}",
                result.err()
            );
            // After install, a package-lock.json should exist
            assert!(
                root.join("package-lock.json").exists(),
                "npm activate hook should create package-lock.json"
            );
        } else {
            assert!(
                result.is_err(),
                "npm activate hook should fail when npm is not available"
            );
        }
    }

    #[test]
    fn npm_workspaces_activate_hook_noop_when_no_repos_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No package.json in any repo
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        let result = integration.activate_hook(&ctx);
        assert!(
            result.is_ok(),
            "npm activate hook should be no-op when no repos detected"
        );
        assert!(
            !root.join("package-lock.json").exists(),
            "no package-lock.json should be created when no repos detected"
        );
    }

    // -----------------------------------------------------------------------
    // cargo-workspace: `cargo generate-lockfile`
    // -----------------------------------------------------------------------

    #[test]
    fn cargo_workspace_activate_hook_runs_cargo_generate_lockfile() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/server/Cargo.toml",
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(root, "github/acme/server/src/lib.rs", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let result = integration.activate_hook(&ctx);
        if which::which("cargo").is_ok() {
            assert!(
                result.is_ok(),
                "cargo activate hook should succeed when cargo is available: {:?}",
                result.err()
            );
            assert!(
                root.join("Cargo.lock").exists(),
                "cargo activate hook should create Cargo.lock"
            );
        } else {
            assert!(
                result.is_err(),
                "cargo activate hook should fail when cargo is not available"
            );
        }
    }

    #[test]
    fn cargo_workspace_activate_hook_noop_when_no_repos_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        let result = integration.activate_hook(&ctx);
        assert!(
            result.is_ok(),
            "cargo activate hook should be no-op when no repos detected"
        );
        assert!(
            !root.join("Cargo.lock").exists(),
            "no Cargo.lock should be created when no repos detected"
        );
    }

    // -----------------------------------------------------------------------
    // uv-workspace: `uv sync`
    // -----------------------------------------------------------------------

    #[test]
    fn uv_workspace_activate_hook_runs_uv_sync() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/server/pyproject.toml",
            "[project]\nname = \"server\"\nversion = \"0.1.0\"\n",
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let result = integration.activate_hook(&ctx);
        if which::which("uv").is_ok() {
            assert!(
                result.is_ok(),
                "uv activate hook should succeed when uv is available: {:?}",
                result.err()
            );
            assert!(
                root.join("uv.lock").exists(),
                "uv activate hook should create uv.lock"
            );
        } else {
            assert!(
                result.is_err(),
                "uv activate hook should fail when uv is not available"
            );
        }
    }

    #[test]
    fn uv_workspace_activate_hook_noop_when_no_repos_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        let result = integration.activate_hook(&ctx);
        assert!(
            result.is_ok(),
            "uv activate hook should be no-op when no repos detected"
        );
        assert!(
            !root.join("uv.lock").exists(),
            "no uv.lock should be created when no repos detected"
        );
    }

    // -----------------------------------------------------------------------
    // pnpm-workspaces: `pnpm install`
    // -----------------------------------------------------------------------

    #[test]
    fn pnpm_workspaces_activate_hook_runs_pnpm_install() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}",
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let result = integration.activate_hook(&ctx);
        if which::which("pnpm").is_ok() {
            assert!(
                result.is_ok(),
                "pnpm activate hook should succeed when pnpm is available: {:?}",
                result.err()
            );
            assert!(
                root.join("pnpm-lock.yaml").exists(),
                "pnpm activate hook should create pnpm-lock.yaml"
            );
        } else {
            assert!(
                result.is_err(),
                "pnpm activate hook should fail when pnpm is not available"
            );
        }
    }

    #[test]
    fn pnpm_workspaces_activate_hook_noop_when_no_repos_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        let result = integration.activate_hook(&ctx);
        assert!(
            result.is_ok(),
            "pnpm activate hook should be no-op when no repos detected"
        );
        assert!(
            !root.join("pnpm-lock.yaml").exists(),
            "no pnpm-lock.yaml should be created when no repos detected"
        );
    }

    // -----------------------------------------------------------------------
    // go-work: no activate hook (default no-op)
    // -----------------------------------------------------------------------

    #[test]
    fn go_work_activate_hook_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        let result = integration.activate_hook(&ctx);
        assert!(result.is_ok(), "go-work activate hook should be a no-op");
        assert!(
            !root.join("go.sum").exists(),
            "go-work activate hook should not create go.sum"
        );
    }

    // -----------------------------------------------------------------------
    // gita: no-op activate hook
    // -----------------------------------------------------------------------

    #[test]
    fn gita_activate_hook_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        let result = integration.activate_hook(&ctx);
        assert!(result.is_ok(), "gita activate hook should be a no-op");
    }

    // -----------------------------------------------------------------------
    // vscode-workspace: no-op activate hook
    // -----------------------------------------------------------------------

    #[test]
    fn vscode_workspace_activate_hook_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        let result = integration.activate_hook(&ctx);
        assert!(
            result.is_ok(),
            "vscode-workspace activate hook should be a no-op"
        );
    }
}

// ===========================================================================
// static-files
// ===========================================================================

mod static_files {
    use super::*;

    #[test]
    fn default_disabled() {
        let integration = StaticFiles;
        assert!(!integration.default_enabled());
    }

    #[test]
    fn name_is_static_files() {
        let integration = StaticFiles;
        assert_eq!(integration.name(), "static-files");
    }

    #[test]
    fn generated_files_returns_configured_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml(
            "enabled: true\nfiles: [turbo.json, .eslintrc.json, .prettierrc]",
        );
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let files = integration.generated_files(&ctx);
        assert_eq!(files, vec!["turbo.json", ".eslintrc.json", ".prettierrc"]);
    }

    #[test]
    fn generated_files_empty_when_no_files_configured() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let files = integration.generated_files(&ctx);
        assert!(files.is_empty());
    }

    #[test]
    fn activate_succeeds_when_files_exist() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create the declared files in the project directory (output_dir)
        write_file(root, "turbo.json", r#"{"pipeline": {}}"#);
        write_file(root, ".eslintrc.json", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config =
            IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json, .eslintrc.json]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let result = integration.activate(&ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn activate_succeeds_even_when_files_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Don't create the files — activate should still succeed (just warn)
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let result = integration.activate(&ctx);
        assert!(
            result.is_ok(),
            "activate should succeed even with missing files"
        );
    }

    #[test]
    fn check_warns_on_missing_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create one of two declared files
        write_file(root, "turbo.json", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config =
            IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json, .eslintrc.json]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.contains(".eslintrc.json"));
        assert_eq!(issues[0].integration, "static-files");
    }

    #[test]
    fn check_no_issues_when_all_files_present() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(root, "turbo.json", "{}");
        write_file(root, ".prettierrc", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config =
            IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json, .prettierrc]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn check_no_issues_when_no_files_configured() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn deactivate_succeeds() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let integration = StaticFiles;
        let result = integration.deactivate(root);
        assert!(result.is_ok());
    }

    #[test]
    fn activate_hook_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let result = integration.activate_hook(&ctx);
        assert!(
            result.is_ok(),
            "static-files activate hook should be a no-op"
        );
    }

    // ----- rwv-c5h: collision with workweave.link (fo-cnpjy.13) -----------

    /// rwv-c5h regression: when the same name is declared in both
    /// `static-files.files` and `workweave.link`, `activate()` MUST bail with a
    /// hard error rather than silently letting the framework's predicate
    /// tiebreak. The error message must name both integrations so the operator
    /// can act on it without re-reading the docs.
    #[test]
    fn activate_fails_when_name_collides_with_workweave_link() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // The static file exists — collision detection runs before existence
        // checks, so we'd rather not give activate() a way to fail for an
        // unrelated reason.
        write_file(root, ".beads", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [.beads]");
        let cache = HashMap::new();
        let workweave = WorkweaveConfig {
            link: vec![".beads".to_string()],
            copy: vec![],
        };
        let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

        let integration = StaticFiles;
        let err = integration.activate(&ctx).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(".beads") && msg.contains("static-files") && msg.contains("workweave"),
            "activate error should name the colliding entry and both integrations; got: {msg}"
        );
    }

    /// rwv-c5h regression: `check()` MUST surface the collision as
    /// `Severity::Error` so `rwv doctor` fails loudly pre-activate (the
    /// signal that motivates the framework predicate).
    #[test]
    fn check_emits_error_for_workweave_link_collision() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [.beads]");
        let cache = HashMap::new();
        let workweave = WorkweaveConfig {
            link: vec![".beads".to_string()],
            copy: vec![],
        };
        let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        let collisions: Vec<_> = issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        assert_eq!(
            collisions.len(),
            1,
            "expected exactly one error-level collision issue, got: {issues:?}"
        );
        let issue = collisions[0];
        assert_eq!(issue.integration, "static-files");
        assert!(
            issue.message.contains(".beads")
                && issue.message.contains("workweave.link")
                && issue.message.contains("static-files.files"),
            "issue should name both integrations and the colliding entry; got: {}",
            issue.message
        );
    }

    /// `check()` emits one Severity::Error per colliding name (not one
    /// aggregated message).
    #[test]
    fn check_emits_one_error_per_collision() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");
        write_file(root, ".secrets", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config =
            IntegrationConfig::from_yaml("enabled: true\nfiles: [.beads, .secrets, turbo.json]");
        let cache = HashMap::new();
        // Two collisions (.beads, .secrets) and one non-collision (turbo.json).
        let workweave = WorkweaveConfig {
            link: vec![".beads".to_string(), ".secrets".to_string()],
            copy: vec![],
        };
        let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        let collisions: Vec<_> = issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        assert_eq!(
            collisions.len(),
            2,
            "expected one Severity::Error per collision, got: {issues:?}"
        );
        // Both colliding names should appear across the issue messages.
        let combined: String = collisions.iter().map(|i| i.message.clone()).collect();
        assert!(combined.contains(".beads"));
        assert!(combined.contains(".secrets"));
    }

    /// No workweave.link at all -> no collision Issues.
    #[test]
    fn check_no_collision_when_workweave_link_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [.beads]");
        let cache = HashMap::new();
        let workweave = WorkweaveConfig {
            link: vec![],
            copy: vec![],
        };
        let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(
            issues.iter().all(|i| i.severity != Severity::Error),
            "no Severity::Error expected when workweave.link is empty, got: {issues:?}"
        );
    }

    /// Disjoint names -> no collision Issues.
    #[test]
    fn check_no_collision_when_names_disjoint() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");
        write_file(root, "turbo.json", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json]");
        let cache = HashMap::new();
        let workweave = WorkweaveConfig {
            link: vec![".beads".to_string()],
            copy: vec![],
        };
        let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(
            issues.iter().all(|i| i.severity != Severity::Error),
            "no Severity::Error expected when names disjoint, got: {issues:?}"
        );
    }

    /// `ctx.workweave == None` -> no collision Issues (projects without a
    /// `workweave:` section in rwv.yaml).
    #[test]
    fn check_no_collision_when_workweave_absent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [.beads]");
        let cache = HashMap::new();
        // make_ctx -> workweave: None
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(
            issues.iter().all(|i| i.severity != Severity::Error),
            "no Severity::Error expected when ctx.workweave is None, got: {issues:?}"
        );
    }

    // -----------------------------------------------------------------------
    // §6 static-files — RED scenarios (fo-cnpjy.14)
    // -----------------------------------------------------------------------
    //
    // §6.static-files.1 (rwv-c5h reproduction) is already covered above by
    // `activate_fails_when_name_collides_with_workweave_link` /
    // `check_emits_error_for_workweave_link_collision` (the C13 hard-error
    // path). This realizes the remaining plan scenarios:
    //
    // §6.static-files.2 — deactivate strips only static-files-owned symlinks;
    // foreign symlinks and user files survive. The integration's
    // `deactivate(root)` is a no-op (symlink removal is the framework's job),
    // so this is the framework-level owner-scoped predicate (C3). RED until
    // the framework predicate is owner-scoped.
    //
    // §6.static-files.3 — missing declared file skipped with warning (already
    // covered by `check_warns_on_missing_files` and
    // `activate_succeeds_even_when_files_missing` above; we leave them in
    // place rather than duplicate).
    //
    // Cross-platform: this scenario uses unix symlinks. Gated `#[cfg(unix)]`.

    /// §6.static-files.2 — deactivate (framework symlink reaping) must remove
    /// only static-files-owned symlinks; workweave.link symlinks and plain
    /// user files survive.
    ///
    /// The framework predicate that owns this (activate.rs:282 owner-blind
    /// removal) is being fixed by C3 (`generated_files()` split into
    /// `managed_files()`) + C13 (owner-scoped removal). Because the predicate
    /// lives in the framework, not in `Integration::deactivate(root)`, this
    /// test asserts the END-STATE behavior via the framework path — for now
    /// we encode the spec as a SKIP'd test with a clear pointer to the
    /// caller. When C3/C13 land they fold the assertion into an e2e test
    /// (plan §8 "e2e (real CLI)").
    #[cfg(unix)]
    #[test]
    #[ignore = "RED: owner-scoped symlink removal is framework-level; \
                turned green by fo-cnpjy.3 + fo-cnpjy.13 via e2e flow. \
                The Integration::deactivate trait method cannot express this \
                — it tests the framework's symlink-reaping predicate, which \
                is the C3/C13 fix."]
    fn s6_static_files_2_deactivate_owner_scoped_symlink_removal() {
        // Encoded here as a placeholder so the §6 inventory is complete and
        // C3/C13 reviewers know where to look. The actual assertion lands
        // when the framework symlink-reaping is callable from this layer
        // (post fo-cnpjy.3) — see plan §8 e2e plan. The intent:
        //
        // GIVEN root with:
        //   .prettierrc      → symlink to projects/<project>/.prettierrc  (static-files-owned)
        //   turbo.json       → symlink to <primary>/turbo.json            (workweave.link)
        //   notes.md         → plain user file (no symlink)
        // WHEN the activation framework reaps symlinks via the owner-scoped
        //      predicate (membership ∈ static-files.files ∧ read_link →
        //      projects/<project>/<file>)
        // THEN
        //   .prettierrc is removed
        //   turbo.json symlink survives (target shape differs)
        //   notes.md is byte-identical
        //
        // The integration's `Integration::deactivate(root)` is a no-op, so
        // exercising this end-to-end requires the framework path that
        // fo-cnpjy.3 + fo-cnpjy.13 deliver. The bead acknowledges this
        // scenario is "extended if needed" — extension lands when the
        // framework-call seam exists.
        panic!(
            "placeholder — fold into e2e under fo-cnpjy.3 + fo-cnpjy.13; \
             see plan §8 e2e plan"
        );
    }
}
