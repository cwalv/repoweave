//! E2E tests for built-in integrations.
//!
//! Each integration is tested for:
//! 1. Auto-detection of relevant repos
//! 2. File generation matching the spec in docs/reference/integrations/index.md
//! 3. Reference repos excluded from generated files
//! 4. Deactivation cleanup
//! 5. Check warnings (e.g., missing tools)
//!
//! The shared common-contract helper lives at `tests/common/contract.rs`.

mod common;

use common::contract;
use repoweave::integration::{Integration, IntegrationContext, Severity};
use repoweave::integrations::*;
use repoweave::manifest::{
    IntegrationConfig, Manifest, ProjectName, RepoPath, Role, WorkweaveConfig,
};
use repoweave::workspace::ContainerKind;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

// ===========================================================================
// Test helpers
// ===========================================================================

/// Build a Manifest with the given repo entries and no integration config overrides.
fn make_manifest(repos: Vec<(&str, Role)>) -> Manifest {
    let mut yaml = String::from("[repositories]\n");
    for (path, role) in &repos {
        let last = path.split('/').next_back().unwrap();
        yaml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"https://github.com/test/{last}.git\"\nversion = \"main\"\nrole = \"{}\"\n",
            role.as_str()
        ));
    }
    Manifest::from_toml_str(&yaml).unwrap()
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
        container_kind: ContainerKind::Primary,
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
/// the static-files / workweave.link collision tests.
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
        container_kind: ContainerKind::Primary,
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

#[cfg(unix)]
fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

#[cfg(unix)]
fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

/// Write a shim named `name` into `bin_dir` that does nothing but exit with
/// `exit_code` — a real binary a child process's PATH can resolve to,
/// standing in for an ecosystem tool. Unlike `std::env::set_var`, which is
/// unsound under a parallel test runner because it mutates process-wide
/// state, this only ever changes the `PATH` of one subprocess this test
/// starts itself.
///
/// The shim is a shebang script the child must find on `PATH` and spawn
/// itself. That is a strictly harder thing to ask for than a git hook: git
/// reads the `#!` line and looks the interpreter up on its own, whereas an
/// ordinary process spawn on Windows does not, and an extensionless file is
/// not a candidate there at all because lookup selects on `PATHEXT`. So this
/// fixture needs both a Windows spelling for the script and a decision about
/// what an executable's name means there before it can port.
#[cfg(unix)]
fn write_exit_code_shim(bin_dir: &Path, name: &str, exit_code: i32) {
    use std::os::unix::fs::PermissionsExt;
    let path = bin_dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nexit {exit_code}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// An `rwv.toml` naming one member and enabling exactly one integration.
///
/// Every other integration is switched off by name rather than left to its
/// default, because several detect the same manifest: a `package.json` member
/// is an npm member and a pnpm member at once, so a fixture that only enables
/// pnpm still runs npm's hook, and on a `PATH` holding neither tool the run
/// fails for the integration the test is not about.
#[cfg(unix)]
fn one_integration_rwv_toml(integration: &str) -> String {
    let mut toml = String::from(
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/server.git\"\nversion = \"main\"\n\
         role = \"owned\"\n",
    );
    for name in [
        "npm-workspaces",
        "pnpm-workspaces",
        "go-work",
        "uv-workspace",
        "cargo-workspace",
        "gita",
        "vscode-workspace",
        "static-files",
    ] {
        toml.push_str(&format!(
            "\n[integrations.{name}]\nenabled = {}\n",
            name == integration
        ));
    }
    toml
}

/// `rwv doctor --json` over a throwaway weave, on a `PATH` that holds `git`
/// and exactly the `tools` named — so whether an ecosystem tool is available
/// is an input to the test rather than a property of the machine.
///
/// The integrations resolve their tool with `which::which`, which answers for
/// the process that calls it, and those calls happen inside the library. A
/// test that calls `check()` in-process therefore cannot decide the answer:
/// the only lever is `PATH`, and mutating that in-process with
/// `std::env::set_var` is unsound under a parallel runner — the reason
/// [`write_exit_code_shim`] spawns a child in the first place. Driving the
/// binary puts the lookup in a child whose `PATH` this test owns.
///
/// `git` is linked through because rwv shells out to it; nothing else is
/// reachable from the child unless `tools` names it.
#[cfg(unix)]
fn doctor_json_on_tool_only_path(
    integration: &str,
    member_manifest: &str,
    manifest_body: &str,
    tools: &[&str],
) -> String {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(ws.join("projects/app")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    write_file(&ws, member_manifest, manifest_body);
    write_file(
        &ws,
        "projects/app/rwv.toml",
        &one_integration_rwv_toml(integration),
    );
    write_file(&ws, ".rwv-active", "app\n");

    std::os::unix::fs::symlink(which::which("git").unwrap(), bin.join("git")).unwrap();
    for tool in tools {
        write_exit_code_shim(&bin, tool, 0);
    }

    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .env("PATH", bin.display().to_string())
        .output()
        .expect("rwv should run");
    String::from_utf8(out.stdout).expect("doctor --json emits utf-8")
}

/// Run `rwv activate` over a throwaway weave whose `PATH` carries `git` and a
/// recording stand-in for `tool`, and report whether activation succeeded and
/// what the hook asked the tool to do.
///
/// The authoring pass runs first because `activate` is a context verb: it
/// never writes the managed file a hook needs, so without it cargo's hook
/// declines before reaching the tool. The witness is cleared afterwards, so
/// what it holds at the end is the audited run alone.
///
/// Recording the invocation is what makes both directions forceable. The old
/// assertions read a lock file, which only a real tool produces — so the
/// success half only ever ran where the tool happened to be installed. An
/// argv pin says the thing the hook actually promises (it reaches the tool,
/// with these arguments) and leaves the tool's own behaviour to `exit_code`.
///
/// `produces` is the output the caller's hook reads back after the tool runs:
/// cargo's records a digest of the lock it just generated, so a stand-in that
/// only exits leaves the hook failing on a file that was never written. Naming
/// the artifact keeps that a property of the fixture rather than of the shim.
#[cfg(unix)]
fn activate_with_tool_shim(
    integration: &str,
    member_manifest: &str,
    manifest_body: &str,
    tool: &str,
    exit_code: i32,
    produces: &[&str],
) -> (bool, String) {
    use std::os::unix::fs::PermissionsExt;

    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let bin = tmp.path().join("bin");
    let witness = tmp.path().join("witness");
    std::fs::create_dir_all(ws.join("projects/app")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    write_file(&ws, member_manifest, manifest_body);
    write_file(
        &ws,
        "projects/app/rwv.toml",
        &one_integration_rwv_toml(integration),
    );
    write_file(&ws, ".rwv-active", "app\n");
    std::os::unix::fs::symlink(which::which("git").unwrap(), bin.join("git")).unwrap();

    let shim = |code: i32| {
        let path = bin.join(tool);
        let mut script = format!("#!/bin/sh\necho \"$@\" >> {}\n", witness.display());
        for artifact in produces {
            script.push_str(&format!("printf '' >> {}\n", ws.join(artifact).display()));
        }
        script.push_str(&format!("exit {code}\n"));
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let run = |args: &[&str]| {
        common::rwv()
            .args(args)
            .current_dir(&ws)
            .env("PATH", bin.display().to_string())
            .output()
            .expect("rwv should run")
    };

    shim(0);
    run(&["doctor", "--fix"]);
    let _ = std::fs::remove_file(&witness);

    shim(exit_code);
    let out = run(&["activate", "app"]);
    let invoked = std::fs::read_to_string(&witness).unwrap_or_default();
    (out.status.success(), invoked)
}

/// Whether `doctor --json` raised `tool-missing` against `integration`.
///
/// Reads the published `kind` out of the parsed report rather than searching
/// its text: an integration's name appears in other findings' messages too, so
/// a substring would answer for the wrong one. `tool-missing` is an *issue*,
/// not a *violation* — the report carries both arrays, and reading the wrong
/// one returns `false` for every input, which reads as "the tool was found".
///
/// Hence the emptiness assertion: a caller asking whether a finding is ABSENT
/// gets a true answer from a broken enumeration just as readily as from a
/// working one, so this refuses to answer over an array it did not find.
#[cfg(unix)]
fn reports_tool_missing(report: &str, integration: &str) -> bool {
    let parsed: serde_json::Value =
        serde_json::from_str(report).expect("doctor --json emits a JSON report");
    let issues = parsed["issues"]
        .as_array()
        .expect("a doctor report carries an issues array");
    assert!(
        !issues.is_empty(),
        "these fixtures always leave managed files unwritten, so an empty issues \
         array means the report was not read: {report}"
    );
    issues
        .iter()
        .any(|v| v["kind"] == "tool-missing" && v["integration"] == integration)
}

// ===========================================================================
// npm-workspaces
// ===========================================================================

mod npm_workspaces {
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
        assert!(
            reports_tool_missing(&absent, "npm-workspaces"),
            "with npm off the child's PATH, doctor must raise tool-missing for \
             npm-workspaces; got:\n{absent}"
        );

        let present = doctor_json_on_tool_only_path(
            "npm-workspaces",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            &["npm"],
        );
        assert!(
            !reports_tool_missing(&present, "npm-workspaces"),
            "with a npm on the child's PATH, the finding must clear; got:\n{present}"
        );
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
}

// ===========================================================================
// pnpm-workspaces
// ===========================================================================

mod pnpm_workspaces {
    use super::*;

    #[test]
    fn auto_detects_repos_with_package_json() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

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

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_pnpm_workspace_yaml_with_packages_list() {
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
        let tmp = common::tempdir().unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(root, "pnpm-workspace.yaml", "packages:\n  - foo\n");
        assert!(root.join("pnpm-workspace.yaml").exists());

        let integration = PnpmWorkspaces;
        integration.deactivate(root).unwrap();
        // No marker → user took the pen → file must survive.
        assert!(root.join("pnpm-workspace.yaml").exists());
    }

    #[cfg(unix)]
    #[test]
    fn check_warns_when_pnpm_not_on_path() {
        let absent = doctor_json_on_tool_only_path(
            "pnpm-workspaces",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            &[],
        );
        assert!(
            reports_tool_missing(&absent, "pnpm-workspaces"),
            "with pnpm off the child's PATH, doctor must raise tool-missing for \
             pnpm-workspaces; got:\n{absent}"
        );

        let present = doctor_json_on_tool_only_path(
            "pnpm-workspaces",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            &["pnpm"],
        );
        assert!(
            !reports_tool_missing(&present, "pnpm-workspaces"),
            "with a pnpm on the child's PATH, the finding must clear; got:\n{present}"
        );
    }

    // -----------------------------------------------------------------------
    // pnpm-workspaces — RED scenarios (turned green by C10)
    // -----------------------------------------------------------------------
    //
    // Synthetic scenarios: no on-disk pnpm-workspace.yaml
    // exists in any weave; the four scenarios use spec idioms (`catalog:`,
    // `overrides:`, `peerDependencyRules:`, `# comments`). default_enabled is
    // false today; tests force it on via `enabled: true` in the config.
    //
    // The pnpm integration uses `default_enabled=false`, but we still call
    // activate/deactivate directly (the integration's own gating logic ignores
    // default_enabled when invoked through trait methods).

    /// Activate preserves a user catalog and comment.
    #[test]
    fn s6_pnpm_1_activate_preserves_catalog_and_comments() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "github/acme/server/package.json");

        // Pre-existing YAML with catalog (user foreign content) + rwv marker on
        // packages (previously-activated state). The catalog and rationale comment
        // must survive activate byte-stable; packages is owned and gets updated.
        write_file(
            root,
            "pnpm-workspace.yaml",
            r#"# shared dependency versions
catalog:
  react: ^18.2.0
  react-dom: ^18.2.0

# managed by repoweave
packages:
  - tools/*
"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true");
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

    /// Deactivate strips `packages:` but keeps `overrides:`.
    /// Regression vs current unconditional remove_file at pnpm_workspaces.rs:33-35.
    #[test]
    fn s6_pnpm_2_deactivate_strips_packages_keeps_overrides() {
        let tmp = common::tempdir().unwrap();
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
            &[contract::substr_probe("server entry", "github/acme/server")],
            &contract::substr_probe("yaml marker", "managed by repoweave"),
            &["overrides:", "lodash@<4.17.21: '>=4.17.21'"],
        );
    }

    /// Deactivate deletes a fully-rwv-authored file (no foreign
    /// content). delete-if-empty kicks in.
    ///
    /// Currently GREEN incidentally — current pnpm deactivate is an
    /// unconditional `remove_file`, which happens to satisfy this scenario.
    /// Keep ungated as a regression guard against the C10 port.
    #[test]
    fn s6_pnpm_3_deactivate_deletes_purely_rwv_file() {
        let tmp = common::tempdir().unwrap();
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

    /// Activate is comment-safe & idempotent. peerDependencyRules
    /// with an inline comment survives byte-for-byte, even when activate runs
    /// twice with a member added in between.
    #[test]
    fn s6_pnpm_4_activate_idempotent_comments_preserved() {
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true");
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
        assert_eq!(
            marker_count, 1,
            "marker must appear exactly once; got:\n{after_second}"
        );

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

    // -----------------------------------------------------------------------
    // Multi-package repo expansion (pnpm uses pnpm-workspace.yaml, not
    // package.json workspaces — mirror of npm expansion tests but reading
    // from `pnpm-workspace.yaml`'s `packages:` key in the member repo)
    // -----------------------------------------------------------------------

    /// A member repo with its own `pnpm-workspace.yaml` declaring sub-package
    /// globs (array form) gets expanded into prefixed entries; the repo root
    /// itself is NOT emitted as an entry.
    #[test]
    fn multi_package_repo_expands_to_prefixed_globs() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // A multi-package repo: root package.json present (triggers detection)
        // and a pnpm-workspace.yaml declaring its own sub-packages.
        touch(root, "github/acme/mono/package.json");
        write_file(
            root,
            "github/acme/mono/pnpm-workspace.yaml",
            "packages:\n  - packages/*\n  - ./clients/ts\n",
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

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        // Prefixed globs from the member repo's pnpm-workspace.yaml.
        assert!(
            content.contains("github/acme/mono/packages/*"),
            "expected prefixed glob; got:\n{content}"
        );
        // Leading './' in member globs is stripped during prefixing.
        assert!(
            content.contains("github/acme/mono/clients/ts"),
            "expected ./ stripped; got:\n{content}"
        );
        // The multi-package repo root itself is NOT listed.
        assert!(
            !content.contains("  - github/acme/mono\n"),
            "repo root must not appear as bare entry; got:\n{content}"
        );
        // Single-package repo keeps the bare path entry.
        assert!(
            content.contains("github/acme/server"),
            "single-package repo must appear; got:\n{content}"
        );
    }

    /// A member repo with its own `pnpm-workspace.yaml` but an empty
    /// `packages:` list is treated as a single-package repo (bare path entry).
    #[test]
    fn multi_package_repo_empty_packages_list_keeps_bare_path() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/mono/package.json");
        write_file(
            root,
            "github/acme/mono/pnpm-workspace.yaml",
            "packages: []\ncatalog:\n  react: ^18\n",
        );

        let manifest = make_manifest(vec![("github/acme/mono", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        // Empty packages list → falls back to bare repo path.
        assert!(
            content.contains("github/acme/mono"),
            "empty packages list must yield bare entry; got:\n{content}"
        );
    }

    /// A member repo without any `pnpm-workspace.yaml` keeps the single
    /// `<repo-path>` entry (existing behavior, no regression).
    #[test]
    fn single_package_repo_no_pnpm_workspace_yaml_keeps_bare_path() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");
        // No pnpm-workspace.yaml in this repo.

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        assert!(
            content.contains("github/acme/server"),
            "single-package repo must appear as bare entry; got:\n{content}"
        );
        // And the root of that repo must NOT have been globbed into sub-entries.
        assert!(
            !content.contains("github/acme/server/"),
            "single-package repo must not produce prefixed sub-entries; got:\n{content}"
        );
    }

    /// This module asserts on `pnpm-workspace.yaml` by searching the whole
    /// file for a member path, and the whole file is not the owned region:
    /// `catalog:` and every comment are user content rwv carries through. So
    /// a path sitting in either is enough to satisfy such a search without
    /// being a workspace member at all.
    ///
    /// Latent exposure, not a live defect — no sibling fixture above puts a
    /// member-shaped path anywhere but the list, so every one of them is
    /// correct today. This is the fixture that tells the two apart, and it
    /// asserts through `verify()`, which compares the on-disk `packages:`
    /// sequence against the manifest rather than searching text.
    #[test]
    fn a_decoy_path_in_a_comment_or_catalog_is_not_a_workspace_member() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pnpm-workspace.yaml",
            "# github/acme/decoy moved out of the weave; note kept on purpose\n\
             catalog:\n  github/acme/decoy: ^1.0.0\n",
        );
        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        PnpmWorkspaces.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        assert!(
            content.contains("github/acme/decoy"),
            "fixture is inert unless the decoy survives activate; got:\n{content}"
        );

        let issues = PnpmWorkspaces.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "the decoy is user content, so the authored packages: list is exactly \
             the manifest's members and verify has nothing to report; got: {issues:?}"
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // go work use requires valid go.mod files (not just empty touches).
        write_file(
            root,
            "github/acme/server/go.mod",
            "module github.com/acme/server\n\ngo 1.21\n",
        );
        write_file(
            root,
            "github/acme/web/go.mod",
            "module github.com/acme/web\n\ngo 1.21\n",
        );
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

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_go_work_with_use_directives() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/chatly/protocol/go.mod",
            "module github.com/chatly/protocol\n\ngo 1.21\n",
        );
        write_file(
            root,
            "github/chatly/server/go.mod",
            "module github.com/chatly/server\n\ngo 1.21\n",
        );

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Owned),
            ("github/chatly/server", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        // New behavior (merge port): the file includes the ownership marker
        // and uses tab-indented `use` blocks (go tool format).
        // Assert structural content rather than exact string (format varies).
        assert!(
            content.contains("./github/chatly/protocol"),
            "protocol path missing: {content}"
        );
        assert!(
            content.contains("./github/chatly/server"),
            "server path missing: {content}"
        );
        assert!(
            content.contains("// managed by repoweave"),
            "marker missing: {content}"
        );
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // go work use requires valid go.mod files.
        write_file(
            root,
            "github/acme/server/go.mod",
            "module github.com/acme/server\n\ngo 1.21\n",
        );
        write_file(
            root,
            "github/acme/reference-lib/go.mod",
            "module github.com/acme/reference-lib\n\ngo 1.21\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project").unwrap();
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
    fn deactivation_removes_go_work_when_marker_present_and_only_rwv_content() {
        // New behavior (merge port): deactivate strips the managed `use` block
        // and deletes the file only when nothing user-authored remains.
        // A file with no marker is left untouched (user holds the pen).
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // File with marker + use block (no replace/toolchain/godebug = "empty").
        write_file(
            root,
            "go.work",
            "go 1.21\n\n// managed by repoweave\nuse (\n\t./github/acme/server\n)\n",
        );
        assert!(root.join("go.work").exists());

        let integration = GoWork;
        integration.deactivate(root).unwrap();
        // File deleted: only go/use content remained.
        assert!(!root.join("go.work").exists());
    }

    #[test]
    fn deactivation_noop_when_no_marker() {
        // User-authored go.work without the rwv marker is left untouched.
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(root, "go.work", "go 1.21\n\nuse (\n)\n");
        assert!(root.join("go.work").exists());

        let integration = GoWork;
        integration.deactivate(root).unwrap();
        // File untouched: no marker present.
        assert!(root.join("go.work").exists());
    }

    #[cfg(unix)]
    #[test]
    fn check_warns_when_go_not_on_path() {
        let absent = doctor_json_on_tool_only_path(
            "go-work",
            "github/acme/server/go.mod",
            "module github.com/acme/server\n\ngo 1.20\n",
            &[],
        );
        assert!(
            reports_tool_missing(&absent, "go-work"),
            "with go off the child's PATH, doctor must raise tool-missing for \
             go-work; got:\n{absent}"
        );

        let present = doctor_json_on_tool_only_path(
            "go-work",
            "github/acme/server/go.mod",
            "module github.com/acme/server\n\ngo 1.20\n",
            &["go"],
        );
        assert!(
            !reports_tool_missing(&present, "go-work"),
            "with a go on the child's PATH, the finding must clear; got:\n{present}"
        );
    }

    // -----------------------------------------------------------------------
    // go-work — RED scenarios (turned green by C11)
    // -----------------------------------------------------------------------
    //
    // A real weave's go.work carries `go 1.26` and a `use(...)` block over
    // its members. The member names here are illustrative: `repoweave` and
    // `some-go-tool` stand in for whatever a given weave actually holds.
    //
    // The hand-parse fallback is mandatory: the merge-logic tests
    // must exercise the fallback deterministically. The current impl always
    // overwrites and does not use `go work edit`, so for now we exercise the
    // hand-parse fallback path implicitly (no `go work edit` exists).
    //
    // s6_go_1 and s6_go_2 pin their go.work/go.mod fixtures at 1.20, not this
    // file's 1.26: both go through activate() with `go` on PATH, and 1.21 is
    // the oldest go release with GOTOOLCHAIN switching, so a fixture at or
    // below that never makes `go work` reach the network for a toolchain
    // download. s6_go_3 and s6_go_4 go through deactivate(), which never
    // invokes `go`, so they keep 1.26.

    /// Adding a repo preserves a hand-authored `replace` directive.
    /// `go 1.20` must NOT be downgraded to `1.21` (the concrete bug).
    #[test]
    fn s6_go_1_add_preserves_replace_and_go_version() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // go.mod files declare go 1.20 to match the go.work version.
        // When go is on PATH (primary path), max_go_version is computed from
        // these files; matching the go.work version prevents a downgrade.
        write_file(
            root,
            "github/cwalv/repoweave/go.mod",
            "module github.com/cwalv/repoweave\n\ngo 1.20\n",
        );
        write_file(
            root,
            "github/cwalv/some-go-tool/go.mod",
            "module github.com/cwalv/some-go-tool\n\ngo 1.20\n",
        );
        write_file(
            root,
            "github/cwalv/another-module/go.mod",
            "module github.com/cwalv/another-module\n\ngo 1.20\n",
        );

        // Pre-existing go.work with go 1.20, two members, a replace + comment.
        write_file(
            root,
            "go.work",
            r#"go 1.20

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
        let project = ProjectName::new("test-project").unwrap();
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
            content.contains("go 1.20"),
            "go 1.20 must survive (NOT downgraded to 1.21); got:\n{content}"
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

    /// Removing a repo strips its use entry but keeps toolchain.
    #[test]
    fn s6_go_2_remove_keeps_toolchain_and_godebug() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        // go.mod files declare go 1.20 to match the go.work version (avoids
        // primary-path downgrade when go is on PATH and max_go_version is computed).
        write_file(
            root,
            "github/cwalv/repoweave/go.mod",
            "module github.com/cwalv/repoweave\n\ngo 1.20\n",
        );
        write_file(
            root,
            "github/cwalv/some-go-tool/go.mod",
            "module github.com/cwalv/some-go-tool\n\ngo 1.20\n",
        );
        // another-module is in the go.work seed but being removed from the manifest.
        // Its go.mod must exist on disk so the primary-path `go work use` for the
        // kept repos succeeds (go validates all existing use entries on modification).
        write_file(
            root,
            "github/cwalv/another-module/go.mod",
            "module github.com/cwalv/another-module\n\ngo 1.20\n",
        );

        write_file(
            root,
            "go.work",
            r#"go 1.20

toolchain go1.20.0

godebug default=go1.20

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
        let project = ProjectName::new("test-project").unwrap();
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
            content.contains("toolchain go1.20.0"),
            "toolchain must survive; got:\n{content}"
        );
        assert!(
            content.contains("godebug default=go1.20"),
            "godebug must survive; got:\n{content}"
        );
        assert!(
            content.contains("go 1.20"),
            "go version must survive; got:\n{content}"
        );
    }

    /// Deactivate strips the use set but keeps replace.
    /// Regression vs current unconditional remove_file at go_work.rs:36-38.
    #[test]
    fn s6_go_3_deactivate_strips_use_keeps_replace() {
        let tmp = common::tempdir().unwrap();
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

    /// Deactivate deletes when only rwv content remained.
    ///
    /// Currently GREEN incidentally — current go.work deactivate is an
    /// unconditional `remove_file`, which happens to satisfy this scenario.
    /// Keep ungated as a regression guard against the C11 port: when C11
    /// switches to strip-not-delete-with-delete-if-empty, this scenario must
    /// still hold (file deleted because the post-strip doc is empty).
    #[test]
    fn s6_go_4_deactivate_deletes_when_only_rwv_content() {
        let tmp = common::tempdir().unwrap();
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

    /// This module asserts on `go.work` by searching the whole file for a
    /// member path, and the whole file is not the owned region: `replace`
    /// directives and every comment are user content rwv carries through. So a
    /// path sitting in either is enough to satisfy such a search without being
    /// a `use` member at all.
    ///
    /// Latent exposure, not a live defect — no sibling fixture above puts a
    /// member-shaped path anywhere but the `use` block, so every one of them is
    /// correct today. This is the fixture that tells the two apart, and it
    /// asserts through `verify()`, which compares the on-disk `use` set against
    /// the manifest rather than searching text.
    ///
    /// Pinned at `go 1.20` for the same reason the scenarios above are: with
    /// `go` on PATH this runs the `go work` path, and 1.21 is the oldest
    /// release that would send it to the network for a toolchain.
    #[test]
    fn a_decoy_path_in_a_comment_or_replace_is_not_a_use_member() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/server/go.mod",
            "module github.com/acme/server\n\ngo 1.20\n",
        );

        write_file(
            root,
            "go.work",
            r#"go 1.20

// ./github/acme/decoy left the weave; the note is kept on purpose
replace example.com/decoy => ./github/acme/decoy
"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        GoWork.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            content.contains("github/acme/decoy"),
            "fixture is inert unless the decoy survives activate; got:\n{content}"
        );

        let issues = GoWork.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "the decoy is user content, so the authored use set is exactly the \
             manifest's members and verify has nothing to report; got: {issues:?}"
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");
        touch(root, "github/acme/web/pyproject.toml");
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

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_pyproject_toml_with_uv_workspace() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/pyproject.toml");
        touch(root, "github/chatly/server/pyproject.toml");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Owned),
            ("github/chatly/server", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        // Post-port: ownership is proven by the per-key `# managed by rwv`
        // decor on the managed key (TomlDoc marker). The legacy
        // `# Generated by rwv` first-line header is gone.
        assert!(
            content.contains("# managed by rwv"),
            "managed-by-rwv decor missing: {content}"
        );
        assert!(
            !content.starts_with("# Generated by rwv"),
            "legacy header must not appear"
        );
        assert!(content.contains("[tool.uv.workspace]"));
        assert!(content.contains("\"github/chatly/protocol\""));
        assert!(content.contains("\"github/chatly/server\""));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");
        touch(root, "github/acme/reference-lib/pyproject.toml");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // A file holding only the marker-decorated managed region is fully
        // rwv-owned: deactivate strips it and deletes the empty file.
        write_file(
            root,
            "pyproject.toml",
            "[tool.uv.workspace]\n# managed by rwv\nmembers = []\n",
        );
        assert!(root.join("pyproject.toml").exists());

        let integration = UvWorkspace;
        integration.deactivate(root).unwrap();
        assert!(!root.join("pyproject.toml").exists());
    }

    #[test]
    fn deactivation_preserves_handwritten_pyproject_toml() {
        let tmp = common::tempdir().unwrap();
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

    #[cfg(unix)]
    #[test]
    fn check_warns_when_uv_not_on_path() {
        let absent = doctor_json_on_tool_only_path(
            "uv-workspace",
            "github/acme/server/pyproject.toml",
            "[project]\nname = \"server\"\nversion = \"0.1.0\"\n",
            &[],
        );
        assert!(
            reports_tool_missing(&absent, "uv-workspace"),
            "with uv off the child's PATH, doctor must raise tool-missing for \
             uv-workspace; got:\n{absent}"
        );

        let present = doctor_json_on_tool_only_path(
            "uv-workspace",
            "github/acme/server/pyproject.toml",
            "[project]\nname = \"server\"\nversion = \"0.1.0\"\n",
            &["uv"],
        );
        assert!(
            !reports_tool_missing(&present, "uv-workspace"),
            "with a uv on the child's PATH, the finding must clear; got:\n{present}"
        );
    }

    // -----------------------------------------------------------------------
    // uv-workspace — scenarios (GREEN)
    // -----------------------------------------------------------------------
    //
    // Seeds use astral-sh/ruff pyproject.toml idioms (maturin + ruff + black +
    // rooster). Marker = per-key `# managed by rwv` decor on
    // `[tool.uv.workspace].members`. Reuses TomlDoc from C7.

    /// Activate preserves a real maturin+ruff root (merge, not clobber).
    #[test]
    fn s6_uv_1_activate_preserves_ruff_style_root() {
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("test-project").unwrap();
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
                contract::substr_probe("members[server]", "github/astral/server"),
                contract::substr_probe("members[web]", "github/astral/web"),
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
        // inject a header into a user file. Both are rejected.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.starts_with("# Generated by rwv"),
            "legacy header must not be at line 1 of a user-authored file; \
             got first line: {:?}",
            text.lines().next()
        );
    }

    /// Add a repo: idempotent, only mutates members; user
    /// `[tool.uv.sources]` entries that aren't `{workspace=true}` survive.
    #[test]
    fn s6_uv_2_add_member_preserves_user_sources() {
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("test-project").unwrap();
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

    /// Deactivate strips only rwv keys, keeps the manifest.
    /// User non-workspace sources survive.
    #[test]
    fn s6_uv_3_deactivate_strips_keeps_user_manifest() {
        let tmp = common::tempdir().unwrap();
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

    /// Greenfield root: rwv creates pyproject.toml from scratch with
    /// `package=false`; deactivate removes the managed region but preserves
    /// `package = false` (it is a `DefaultOnly` key — user-adjustable, never
    /// stripped). The file survives deactivation with only the DefaultOnly key.
    #[test]
    fn s6_uv_4_greenfield_create_then_deactivate_preserves_package_false() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "github/astral/protocol/pyproject.toml");

        // No root pyproject.toml.
        assert!(!root.join("pyproject.toml").exists());

        let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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

        // package = false is DefaultOnly — it is NOT stripped on deactivate.
        // The file survives with only the DefaultOnly key; it is not deleted.
        assert!(
            root.join("pyproject.toml").exists(),
            "file must survive deactivate — package=false (DefaultOnly) was not stripped"
        );
        let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(
            after.contains("package = false") || after.contains("package=false"),
            "package=false must survive deactivate (DefaultOnly key); got:\n{after}"
        );
        // The managed region (members, marker) must be gone.
        assert!(
            !after.contains("managed by rwv"),
            "marker must be removed on deactivate; got:\n{after}"
        );
        assert!(
            !after.contains("members"),
            "members key must be stripped on deactivate; got:\n{after}"
        );
    }

    // -----------------------------------------------------------------------
    // DefaultOnly regression tests for [tool.uv].package
    // -----------------------------------------------------------------------

    /// Regression — user opt-in: file with marker present + user has set
    /// `[tool.uv].package = true` → after activate, stays `true`.
    /// `DefaultOnly` must never overwrite an existing value.
    #[test]
    fn default_only_does_not_overwrite_user_set_package_true() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "github/astral/protocol/pyproject.toml");

        // Pre-existing pyproject.toml with marker on members + user-set package = true.
        write_file(
            root,
            "pyproject.toml",
            concat!(
                "[tool.uv.workspace]\n",
                "# managed by rwv\n",
                "members = [\"github/astral/protocol\"]\n",
                "\n",
                "[tool.uv]\n",
                "package = true\n",
            ),
        );

        let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        UvWorkspace.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(
            after.contains("package = true"),
            "user-set package=true must survive activate (DefaultOnly never overwrites); got:\n{after}"
        );
        assert!(
            !after.contains("package = false"),
            "DefaultOnly must not inject package=false when key is present; got:\n{after}"
        );
    }

    /// Regression — greenfield: fresh file (no root pyproject.toml) gets
    /// `[tool.uv].package = false` set by DefaultOnly.
    #[test]
    fn default_only_sets_package_false_on_greenfield() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "github/astral/protocol/pyproject.toml");

        // No root pyproject.toml — greenfield.
        assert!(!root.join("pyproject.toml").exists());

        let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        UvWorkspace.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(
            after.contains("package = false") || after.contains("package=false"),
            "greenfield file must get package=false from DefaultOnly; got:\n{after}"
        );
    }

    /// Regression — pre-existing without override: file with marker + no
    /// `[tool.uv].package` key → DefaultOnly sets `false`.
    #[test]
    fn default_only_sets_package_false_when_key_absent() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "github/astral/protocol/pyproject.toml");

        // File exists, has marker on members, but no package key.
        write_file(
            root,
            "pyproject.toml",
            concat!(
                "[tool.uv.workspace]\n",
                "# managed by rwv\n",
                "members = [\"github/astral/protocol\"]\n",
            ),
        );

        let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        UvWorkspace.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(
            after.contains("package = false") || after.contains("package=false"),
            "pre-existing file without package key must get package=false from DefaultOnly; got:\n{after}"
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");
        touch(root, "github/acme/web/Cargo.toml");
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

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_cargo_toml_with_workspace_section() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/Cargo.toml");
        touch(root, "github/chatly/server/Cargo.toml");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Owned),
            ("github/chatly/server", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // The legacy `# Generated by rwv ...` first-line header is gone
        // post-port. Ownership is proven by the per-key
        // `# managed by rwv` decor on each owned key (TomlDoc marker).
        assert!(
            content.contains("# managed by rwv"),
            "managed-by-rwv decor missing: {content}"
        );
        assert!(content.contains("[workspace]"));
        assert!(content.contains("\"github/chatly/protocol\""));
        assert!(content.contains("\"github/chatly/server\""));
        assert!(content.contains("resolver = \"2\""));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");
        touch(root, "github/acme/reference-lib/Cargo.toml");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Post-port: ownership is proven by the per-key
        // `# managed by rwv` decor, not a first-line header. Seed a Cargo.toml
        // shaped the way rwv would author it.
        //
        // `resolver` is now Ownership::DefaultOnly — it is NOT
        // stripped on deactivate. A file with only `members` (Author) and
        // `resolver` (DefaultOnly) will, after deactivate, still contain
        // `resolver = "2"`, so the file is NOT deleted (it has remaining
        // content). Test updated to assert the file exists and only members
        // was stripped.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = []\n# managed by rwv\nresolver = \"2\"\n",
        );
        assert!(root.join("Cargo.toml").exists());

        let integration = CargoWorkspace;
        integration.deactivate(root).unwrap();

        // File should still exist: resolver (DefaultOnly) was not stripped.
        assert!(
            root.join("Cargo.toml").exists(),
            "Cargo.toml must not be deleted when resolver (DefaultOnly) content remains"
        );
        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // members (Author) was stripped.
        assert!(
            !content.contains("members"),
            "members (Author key) should be stripped on deactivate: {content}"
        );
        // resolver (DefaultOnly) was NOT stripped.
        assert!(
            content.contains("resolver"),
            "resolver (DefaultOnly) must survive deactivate: {content}"
        );
    }

    #[test]
    fn deactivation_preserves_handwritten_cargo_toml() {
        let tmp = common::tempdir().unwrap();
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

    #[cfg(unix)]
    #[test]
    fn check_warns_when_cargo_not_on_path() {
        let absent = doctor_json_on_tool_only_path(
            "cargo-workspace",
            "github/acme/server/Cargo.toml",
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            &[],
        );
        assert!(
            reports_tool_missing(&absent, "cargo-workspace"),
            "with cargo off the child's PATH, doctor must raise tool-missing for \
             cargo-workspace; got:\n{absent}"
        );

        let present = doctor_json_on_tool_only_path(
            "cargo-workspace",
            "github/acme/server/Cargo.toml",
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            &["cargo"],
        );
        assert!(
            !reports_tool_missing(&present, "cargo-workspace"),
            "with a cargo on the child's PATH, the finding must clear; got:\n{present}"
        );
    }

    // -----------------------------------------------------------------------
    // Nested-workspace handling
    // -----------------------------------------------------------------------

    #[test]
    fn nested_workspace_without_opt_out_fails_with_named_repo_error() {
        // A member repo declares its own [workspace]. Activation must fail
        // before any cargo invocation, naming the conflicting repo and
        // listing the three resolutions.
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("test-project").unwrap();
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
    fn nested_workspace_with_opt_out_drops_repo_from_members() {
        // With the opt-out set, the conflicting repo is dropped from members.
        // Post-port: the `# excluded:` comment emitted by the
        // legacy whole-overwrite path is gone. The merge model owns *keys*,
        // not free-floating comments — TomlDoc has no comment-emission API
        // and the helper's contract is "never author outside the owned-key
        // set". Operators see exclusions in rwv.toml directly; surfacing
        // them in the file was decorative.
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("exclude = [\"github/cwalv/forked\"]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
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
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("test-project").unwrap();
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
        // complain. (No-op fallback per spec.)
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml(
            "exclude = [\"github/cwalv/docs-only\", \"github/missing/repo\"]",
        );
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/cwalv/forked/Cargo.toml",
            "[workspace]\nmembers = [\"crate-a\"]\n",
        );

        let manifest = make_manifest(vec![("github/cwalv/forked", Role::Fork)]);
        let project = ProjectName::new("test-project").unwrap();
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
    // cargo-workspace — real-world manifest scenarios
    // -----------------------------------------------------------------------
    //
    // Seeds use real idioms from two published Cargo workspaces rather than
    // minimal fixtures: a NOTE comment block, `profile.*` panic="abort" and
    // `workspace.lints.clippy`; and `workspace.dependencies`,
    // `workspace.package`, `profile.release.package.<crate>` and
    // `workspace.lints.rust`. The merge must leave every one of them
    // byte-stable, which a synthetic manifest would not exercise.

    /// Activate preserves `[profile.*]` + `[workspace.lints]`.
    /// Seed file: previously-activated state (per-key `# managed by rwv` on
    /// members/resolver) plus the seed idioms above. After re-activate, the NOTE
    /// comment block, panic="abort", and clippy deny policy must all survive
    /// byte-stable.
    #[test]
    fn s6_1_activate_preserves_rvtty_profiles_and_lints() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Real Rust repos that the integration will detect as members.
        touch(root, "github/acme/rvtty-a/Cargo.toml");
        touch(root, "github/acme/rvtty-b/Cargo.toml");

        // Seed the root Cargo.toml in previously-activated shape:
        //   - per-key `# managed by rwv` decoration on members and resolver
        //     (the port's ownership marker; legacy `# Generated by rwv` header is gone)
        //   - rvtty idioms (NOTE block, profile panic=abort, workspace.lints.clippy)
        //     as user foreign content that must round-trip untouched.
        write_file(
            root,
            "Cargo.toml",
            r#"#
# NOTE (olb.5.4): rvtty-style hand-maintained block. profile/lint policy
# must round-trip activate untouched. This comment block is part of the
# regression — strip it and the rationale is lost forever.

[workspace]
# managed by rwv
members = []
# managed by rwv
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
        let project = ProjectName::new("test-project").unwrap();
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
            // Marker: per-key `# managed by rwv` decor on `members`.
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

    /// Re-activate is idempotent w.r.t. `[workspace.dependencies]`
    /// / `[workspace.package]` / `[profile.*]` (the ruff surface).
    #[test]
    fn s6_2_reactivate_idempotent_over_ruff_surface() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/astral/ruff/Cargo.toml");
        touch(root, "github/astral/ty/Cargo.toml");

        // Ruff-idiom previously-activated root: per-key `# managed by rwv` on
        // members and resolver, plus the full ruff surface (workspace.package,
        // deps, lints, profile.*) as user foreign content.
        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
# managed by rwv
members = ["github/astral/ruff"]
# managed by rwv
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
        let project = ProjectName::new("test-project").unwrap();
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

    /// Deactivate strips only Author keys, keeps user policy and
    /// DefaultOnly keys.
    ///
    /// `resolver` is now Ownership::DefaultOnly — it is NOT stripped
    /// on deactivate. Only `members` (Author) is stripped. `resolver` survives
    /// along with the rest of the user's content.
    #[test]
    fn s6_3_deactivate_strips_keeps_user_policy() {
        let tmp = common::tempdir().unwrap();
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
        // Only `members` (Ownership::Author) is an owned probe that must be
        // absent after deactivate. `resolver` (Ownership::DefaultOnly) survives
        // and is listed in foreign_substrings below.
        contract::assert_deactivate_strips_keeps(
            &path,
            || {
                CargoWorkspace.deactivate(root).unwrap();
            },
            &[contract::substr_probe(
                "members entry",
                "github/acme/server",
            )],
            &contract::substr_probe("toml marker", "managed by rwv"),
            &[
                // resolver (DefaultOnly) must survive deactivate.
                "resolver = \"2\"",
                "[profile.dev]",
                "panic = \"abort\"",
                "[workspace.lints.clippy]",
                "dbg_macro = \"deny\"",
                "[patch.crates-io]",
                "foo = { path = \"vendor/foo\" }",
            ],
        );
    }

    /// Members sub-path config + nested-workspace exemption.
    /// Repo with no root Cargo.toml; config emits
    /// `<repo>/<sub>` per include. Sibling workspace is NOT an ancestor and
    /// must NOT trip the nested-workspace error.
    ///
    /// Members sub-path config is added by C6 (cargo design-finalization) +
    /// C8 (cargo members-subpath + [patch] opt-in).
    #[test]
    fn s6_4_members_subpath_and_nested_workspace_exemption() {
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("test-project").unwrap();
        // The members-subpath config shape: per-repo sub-path include list.
        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\", \"common\"]\n",
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

    /// Opt-in `[patch.crates-io]` generation for
    /// cross-repo path deps. With `integrations.cargo-workspace.patch: true`
    /// rwv scans each member's `Cargo.toml` for `path = "..."` deps that
    /// point into another known member, and emits a
    /// `[patch.crates-io].<crate>` entry keyed by the target crate's name.
    #[test]
    fn patch_opt_in_emits_crates_io_entries_for_cross_repo_path_deps() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Two repos. `app` depends on `lib` via a relative `path = ...`.
        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            r#"[package]
name = "acme-app"
version = "0.1.0"
edition = "2021"

[dependencies]
acme-lib = { path = "../lib" }
"#,
        );

        let manifest = make_manifest(vec![
            ("github/acme/lib", Role::Owned),
            ("github/acme/app", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = true\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("[patch.crates-io"),
            "expected a `[patch.crates-io]` section; got:\n{content}"
        );
        assert!(
            content.contains("acme-lib"),
            "expected a patch entry keyed by the dep's crate name `acme-lib`; got:\n{content}"
        );
        assert!(
            content.contains("github/acme/lib"),
            "expected the patch entry to point at the lib member; got:\n{content}"
        );
        // The rwv marker should decorate the generated patch entry.
        assert!(
            content.contains("managed by rwv"),
            "expected the rwv marker on managed keys; got:\n{content}"
        );
    }

    /// When `patch` is the default (false), no `[patch]` table
    /// is generated even when cross-repo path deps exist. This is the
    /// internal-crate path: operators commit the relative `path=` dep and
    /// rwv stays out of `[patch]` entirely.
    #[test]
    fn patch_default_false_emits_no_patch_table() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            r#"[package]
name = "acme-app"
version = "0.1.0"
edition = "2021"

[dependencies]
acme-lib = { path = "../lib" }
"#,
        );

        let manifest = make_manifest(vec![
            ("github/acme/lib", Role::Owned),
            ("github/acme/app", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        // Default config: patch defaults to false.
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            !content.contains("[patch"),
            "no `[patch]` table should be generated with patch=false; got:\n{content}"
        );
    }

    /// Deactivate strips rwv-authored `[patch.crates-io]`
    /// entries; user-authored entries survive — deactivate goes through the
    /// same toml_edit merge activate does. (Co-requisite of the activate-time
    /// generation.)
    #[test]
    fn deactivate_strips_rwv_patch_entries_keeps_user_entries() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Seed a Cargo.toml that mixes an rwv-managed [patch.crates-io].acme-lib
        // entry (carrying the marker decor) with a hand-authored
        // [patch.crates-io].vendor-foo entry. Also exercise the [workspace]
        // strip-deactivate path coexists with the new patch strip.
        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
# managed by rwv
members = ["github/acme/app", "github/acme/lib"]
# managed by rwv
resolver = "2"

[patch.crates-io]
# managed by rwv
acme-lib = { path = "github/acme/lib" }
vendor-foo = { git = "https://example.com/vendor-foo" }
"#,
        );

        CargoWorkspace.deactivate(root).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // rwv-authored patch entry is gone.
        assert!(
            !content.contains("acme-lib"),
            "rwv-authored patch entry should be stripped; got:\n{content}"
        );
        // User-authored patch entry survives.
        assert!(
            content.contains("vendor-foo"),
            "user-authored patch entry should survive; got:\n{content}"
        );
        // The user-authored [patch.crates-io] table itself survives (because
        // it's non-empty after the rwv strip).
        assert!(
            content.contains("[patch.crates-io]"),
            "user-authored [patch.crates-io] should survive; got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // PatchMode::Derived (registry-dep tier)
    //
    // Rationale is in src/integrations/cargo_workspace.rs's top-doc comment.
    //
    // The tests below exercise the derived-mode invariants:
    //
    // - Registry-dep matched by name → patch emitted.
    // - `committed-paths` mode is UNCHANGED under a member declaring a
    //   path dep (regression guard on the mirror behavior).
    // - `derived` with a reference-role repo hosting the crate → patched
    //   from the reference-repo path.
    // - Git-source dep → `[patch."<url>"]` entry (not crates-io).
    // - Member `.cargo/config.toml` shadowing key → warning surfaced,
    //   output still correct.
    // - Unpublished in-weave crate is documented tier-boundary behavior,
    //   not a failure — asserted here so the shape is regression-tested.
    // -----------------------------------------------------------------------

    /// A member declaring `<in-weave-crate> = "<req>"` as a registry dep
    /// gets patched to the in-weave path when the crate name matches.
    /// This is what derived mode exists for: sovereign members declare bare
    /// registry versions and the weave supplies the live sources.
    #[test]
    fn derived_patch_matches_registry_dep_by_name() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // `acme-lib` at v0.3.5 in-weave; `acme-app` depends on it as a
        // registry dep (no path=, no git=). Committed-paths mode would
        // emit nothing (there's no cross-repo `path=` to mirror);
        // derived mode should notice the name match and patch.
        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("[patch.crates-io"),
            "derived mode must emit [patch.crates-io] for a matched \
             registry dep; got:\n{content}"
        );
        assert!(
            content.contains("acme-lib"),
            "expected patch entry keyed by the target crate name \
             `acme-lib`; got:\n{content}"
        );
        assert!(
            content.contains("github/acme/lib"),
            "expected patch entry to point at the in-weave lib member; \
             got:\n{content}"
        );
        assert!(
            content.contains("managed by rwv"),
            "generated patch entry must carry the rwv marker; got:\n{content}"
        );
    }

    /// A member with a committed cross-member `path=` dep continues to
    /// receive the mirror patch under `patch: committed-paths` — the
    /// enum rename is a pure rename, existing manifests keep working.
    ///
    /// This test uses the modern string spelling (`committed-paths`) to
    /// exercise the parse path; the boolean back-compat is covered in
    /// manifest_test.rs.
    #[test]
    fn committed_paths_mode_unchanged_for_path_dep_member() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = { path = \"../lib\" }\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"committed-paths\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // Same as the pre-rename `patch_opt_in_emits_crates_io_entries_for_cross_repo_path_deps`
        // assertions — the mirror behavior is unchanged.
        assert!(
            content.contains("[patch.crates-io"),
            "committed-paths mode must emit [patch.crates-io] for a \
             cross-repo path dep; got:\n{content}"
        );
        assert!(content.contains("acme-lib"), "got:\n{content}");
        assert!(content.contains("github/acme/lib"), "got:\n{content}");
        assert!(content.contains("managed by rwv"), "got:\n{content}");
    }

    /// A `reference`-role repo is excluded from the workspace `members`
    /// list (they're read-only study material), but under derived mode
    /// they participate in the patch *index*: an active member declaring
    /// a registry dep whose name matches a reference-repo crate gets
    /// patched to the reference-repo path.
    ///
    /// Reference-role repos are symlinked to the canonical clone in
    /// workweaves; symlinks keep logical paths in `cargo metadata` with zero
    /// rebuild churn, so they are safe patch sources.
    #[test]
    fn derived_patch_includes_reference_repos_as_sources() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Reference-role repo hosts the crate.
        write_file(
            root,
            "github/upstream/lib/Cargo.toml",
            "[package]\nname = \"upstream-lib\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        // Active member depends on it as a registry dep.
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nupstream-lib = \"1.0\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/upstream/lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();

        // Reference repo must NOT appear as a workspace member (parse
        // the members array precisely — the reference path DOES appear
        // as a patch-entry `path = "..."` value, so a substring check
        // is not sufficient).
        let doc: toml_edit::DocumentMut = content.parse().expect("valid TOML");
        let members: Vec<String> = doc
            .get("workspace")
            .and_then(|i| i.as_table())
            .and_then(|t| t.get("members"))
            .and_then(|i| i.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !members.contains(&"github/upstream/lib".to_string()),
            "reference-role repo must not be a workspace member; got: {members:?}"
        );

        // But it MUST appear as the patch source.
        assert!(
            content.contains("upstream-lib"),
            "reference-role repo must be usable as a derived patch source; \
             got:\n{content}"
        );
        assert!(
            content.contains("github/upstream/lib"),
            "patch path must point at the reference-repo dir; got:\n{content}"
        );
    }

    /// A member declaring `foo = { git = "<url>" }` produces a
    /// `[patch."<url>"]` sub-table entry — NOT `[patch.crates-io]`.
    /// Cargo treats git-source deps as a distinct source; a
    /// `[patch.crates-io]` entry does not patch a git-source dep.
    #[test]
    fn derived_patch_emits_git_url_subtable_for_git_source_dep() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\n\
             acme-lib = { git = \"https://example.com/acme/lib.git\", version = \"0.1\" }\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("\"https://example.com/acme/lib.git\""),
            "git-source dep must produce [patch.\"<git-url>\"] entry; \
             got:\n{content}"
        );
        assert!(
            content.contains("acme-lib"),
            "patch entry must be keyed by the target crate name; got:\n{content}"
        );
        // The [patch.crates-io] table must NOT carry this crate — the git
        // source is a distinct source from crates.io. It's OK if the
        // [patch.crates-io] table is absent entirely.
        let after_crates_io = content.split("[patch.crates-io").nth(1).unwrap_or("");
        assert!(
            !after_crates_io.contains("acme-lib"),
            "git-source dep must NOT be patched via [patch.crates-io]; \
             got:\n{content}"
        );
    }

    /// A member's own `.cargo/config.toml` declaring the same
    /// `[patch.crates-io].<crate>` key silently defeats the weave-level
    /// patch — cargo merges `.cargo/config.toml` closest-config-wins per key,
    /// and its diagnostic actively misleads about which one won.
    ///
    /// Derived mode must surface a warning to stderr at generation time
    /// AND emit the patch anyway (the operator may still resolve the
    /// shadowing after being informed). The absence of the warning
    /// wouldn't fail the write, but the write must remain correct.
    #[test]
    fn derived_patch_surfaces_shadowing_warning_and_writes_output() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );
        // Shadowing config.
        write_file(
            root,
            "github/acme/app/.cargo/config.toml",
            "[patch.crates-io]\nacme-lib = { path = \"/wrong/place\" }\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        // The write still lands — the warning is advisory, not gating.
        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("[patch.crates-io"),
            "derived-mode output must land even when a member config \
             shadows the key; got:\n{content}"
        );
        assert!(
            content.contains("acme-lib") && content.contains("github/acme/lib"),
            "patch entry must be correct; got:\n{content}"
        );
        // The stderr channel is not captured here (integration tests
        // don't easily assert on eprintln); the shadowing-detection
        // helper is unit-covered via scan_patch_shadowing_against_keys.
    }

    /// The tier boundary: a member depending on an
    /// *unpublished* crate name (one that only exists in the weave)
    /// resolves only inside the weave — standalone `cargo build` would
    /// fail with "no matching package".
    ///
    /// Derived mode's job is to make the in-weave path work — and it
    /// does, by emitting the patch — but rwv makes no claim about
    /// standalone-buildability of the member.  This is documented
    /// behavior, not a bug. Assert that the shape (patch emitted, no
    /// warning) matches the documented expectation, so any future change
    /// to the tier boundary is caught.
    #[test]
    fn derived_patch_tier_boundary_unpublished_crate_still_patches() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // `weave-only-lib` is a name that (by design) does not exist on
        // crates.io. The member declares it as a bare registry dep;
        // derived mode's index still matches it and emits the patch.
        write_file(
            root,
            "github/acme/weave-only/Cargo.toml",
            "[package]\nname = \"weave-only-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nweave-only-lib = \"0.1\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/weave-only", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("weave-only-lib"),
            "derived-mode makes the in-weave resolution work even for \
             names that don't exist on crates.io — the tier boundary \
             is a member-standalone concern, not a weave concern; \
             got:\n{content}"
        );
        assert!(
            content.contains("github/acme/weave-only"),
            "expected the patch to point at the in-weave path; got:\n{content}"
        );
    }

    /// A member declaring `foo = "1.0"` when the in-weave `foo` is
    /// version `2.0.0` must NOT be patched — cargo would hard-error
    /// with a misleading "location searched: crates.io index" message.
    /// Derived mode catches the mismatch upfront and
    /// simply omits the patch (an eprintln warning surfaces the reason
    /// to the operator).
    #[test]
    fn derived_patch_skips_incompatible_version() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"2.0.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"1.0\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // The [patch.crates-io] table must NOT carry `acme-lib` — the
        // in-weave version (2.0.0) does not satisfy the member's req
        // (1.0). It's OK if the table is absent entirely.
        assert!(
            !content.contains("acme-lib"),
            "incompatible in-weave version must skip patch emission; \
             got:\n{content}"
        );
    }

    /// A member depending on itself under its own crate name (weird but
    /// exercisable in fixtures) must not produce a self-patch — cargo
    /// would reject a patch pointing at the same manifest that declares
    /// the dep.
    #[test]
    fn derived_patch_skips_self_patch() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Contrived: `foo` depends on `foo`. Not realistic, but the
        // guard is a correctness invariant.
        write_file(
            root,
            "github/acme/foo/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nfoo = \"0.1\"\n",
        );

        let manifest = make_manifest(vec![("github/acme/foo", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // No [patch.crates-io] should be generated.
        assert!(
            !content.contains("[patch.crates-io"),
            "self-patches must be skipped; got:\n{content}"
        );
    }

    /// Derived mode must resolve `dep.workspace = true` when the
    /// workspace-deps table is reachable via the same discovery the
    /// version-skew scanner uses.
    ///
    /// Note on fixture shape: cargo's activation-time nested-workspace
    /// hard-error blocks the common grok-build shape (repo root has
    /// `[workspace.dependencies]`) at the weave-workspace level. The
    /// `classify_dep` unit test in `cargo_workspace.rs` covers the
    /// resolve path in isolation; here we exercise the fall-through
    /// path — a member using `workspace = true` with **no** reachable
    /// workspace-deps table must produce no derived patch (silent
    /// skip, no panic).
    #[test]
    fn derived_patch_workspace_true_without_anchor_skips_silently() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        // A member with no reachable workspace-deps table declares its
        // dep as `workspace = true`. This is a broken manifest at cargo
        // load time — but derived-mode must not panic; it must simply
        // skip the entry.
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = { workspace = true }\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Must not panic. The output should carry no patch entry for
        // acme-lib — the workspace = true dep is uninterpretable, so
        // derived-mode skips it. Cargo would refuse the manifest itself
        // at generate-lockfile time.
        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            !content.contains("acme-lib"),
            "unresolvable workspace = true dep must not produce a patch; \
             got:\n{content}"
        );
    }

    /// User-authored `[patch.crates-io].<crate>` entries survive derived
    /// mode — rwv's verify-and-warn semantics apply the same as in
    /// committed-paths mode.
    #[test]
    fn derived_patch_verify_and_warn_preserves_user_authored_entry() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Seed a Cargo.toml with a user-authored patch entry the derived
        // pass would otherwise want to write.
        write_file(
            root,
            "Cargo.toml",
            "[patch.crates-io]\nacme-lib = { git = \"https://user.example.com/acme-lib.git\" }\n",
        );
        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.1\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // User-authored entry survives (unchanged).
        assert!(
            content.contains("https://user.example.com/acme-lib.git"),
            "user-authored patch entry must survive; got:\n{content}"
        );
        // No `# managed by rwv` marker on the user entry (would be a
        // sign that we blew it away).
        //
        // The user's patch line and rwv's would land next to each other;
        // check that the user's URL is present and that we didn't
        // silently overwrite it with a `path = ...`.
        assert!(
            !content.contains("path = \"github/acme/lib\""),
            "user-authored acme-lib entry must not be overwritten by \
             derived; got:\n{content}"
        );
    }

    /// `patch: off` (default) and `patch: derived` with no matched deps
    /// must both emit no `[patch]` table — noise-free. The activation
    /// path must not create an empty table under `derived` just because
    /// the mode is on.
    #[test]
    fn derived_patch_off_and_no_match_emit_no_table() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/foo/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nserde = \"1.0\"\n",
        );

        let manifest = make_manifest(vec![("github/acme/foo", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            !content.contains("[patch"),
            "no `[patch]` table should be generated when no members are \
             in-weave for the declared deps; got:\n{content}"
        );
    }

    /// Deactivate strips derived-mode `[patch."<git-url>"]` entries the
    /// same way it strips `[patch.crates-io]` entries — the strip pass
    /// enumerates every `[patch.<registry>]` sub-table, not just
    /// crates-io.
    #[test]
    fn deactivate_strips_derived_git_patch_entries() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
# managed by rwv
members = ["github/acme/app"]
# managed by rwv
resolver = "2"

[patch."https://example.com/acme/lib.git"]
# managed by rwv
acme-lib = { path = "github/acme/lib" }
"#,
        );

        CargoWorkspace.deactivate(root).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // The git-URL patch table with a single rwv-authored entry
        // should be fully pruned.
        assert!(
            !content.contains("acme-lib"),
            "rwv-authored git-URL patch entry should be stripped; got:\n{content}"
        );
        assert!(
            !content.contains("[patch."),
            "empty [patch.\"<url>\"] table should be pruned; got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // PatchSurface::CargoConfig (nesting-immune lens)
    //
    // The whole point here is the nested-workspace case: cargo hard-errors
    // when it discovers a nested `[workspace]` inside another workspace
    // member, so those repos must opt out of the weave workspace — and once
    // they do, the workspace-manifest `[patch]` NEVER reaches their builds.
    // The `.cargo/config.toml` surface is discovered by upward walk from cwd
    // instead of by workspace membership, so it reaches the opt-out.
    //
    // Assertions here are structural — upward config discovery, relative
    // paths, hybrid-managed ownership; a live-cargo assertion of "this
    // patch would apply" against a nested-workspace fixture lives in
    // e2e_cargo_test.rs — the boundary follows how existing derived-mode
    // tests split (unit-level structure here, cargo-invoked structure there).
    // -----------------------------------------------------------------------

    /// Activate under `patch-surface: cargo-config` writes patches into
    /// `.cargo/config.toml` (NOT the manifest) and rewrites weave-relative
    /// paths to `../<member>` so they resolve against `.cargo/`'s logical
    /// location.
    #[test]
    fn cargo_config_surface_writes_patches_to_dot_cargo() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config =
            IntegrationConfig::from_toml("patch = \"derived\"\npatch-surface = \"cargo-config\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        // The managed Cargo.toml exists but MUST NOT carry [patch.*] —
        // the surface routes elsewhere.
        let manifest_content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            !manifest_content.contains("[patch"),
            "cargo-config surface must not write `[patch]` into the manifest; got:\n{manifest_content}"
        );

        // The .cargo/config.toml file MUST exist and carry the patch entry.
        let config_path = root.join(".cargo").join("config.toml");
        assert!(
            config_path.exists(),
            "cargo-config surface must generate .cargo/config.toml"
        );
        let config_content = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            config_content.contains("[patch.crates-io"),
            "generated config must carry [patch.crates-io]; got:\n{config_content}"
        );
        assert!(
            config_content.contains("acme-lib"),
            "generated config must carry the acme-lib key; got:\n{config_content}"
        );
        // Path stays weave-root-relative: `github/acme/lib` (no `../`).
        // Cargo resolves relative patch paths against the PARENT of
        // `.cargo/` (not `.cargo/` itself — measured directly
        // 2026-07-17). Our `.cargo/` sits directly under the weave root,
        // so a weave-root-relative path resolves the same as it does
        // on the manifest surface. NO canonicalization — resolving the
        // logical path would break a symlinked `.cargo/`.
        assert!(
            config_content.contains("\"github/acme/lib\""),
            "path must be weave-root-relative `github/acme/lib` — cargo \
             resolves patch paths in .cargo/config.toml against the parent \
             of `.cargo/`; got:\n{config_content}"
        );
        // Cross-check: no accidental `../` prefix.
        assert!(
            !config_content.contains("../github/acme/lib"),
            "path must NOT carry a `../` prefix; got:\n{config_content}"
        );
        // The rwv marker decorates the entry (same hybrid ownership as
        // the manifest surface).
        assert!(
            config_content.contains("managed by rwv"),
            "generated entry must carry rwv marker; got:\n{config_content}"
        );
    }

    /// The nesting-immune case: the derived-mode scan
    /// finds a registry dep in an ACTIVE weave member; the patch lands in
    /// `.cargo/config.toml`; a nested-workspace opt-out present in the
    /// same weave picks up the patch when built from inside its dir via
    /// upward config discovery.
    ///
    /// This is the shape a real weave would have: an active member (say,
    /// `chatly/server`) uses `acme-lib = "1.0"` as a registry dep, and a
    /// nested-workspace opt-out (rvtty, mcp_agent_mail_rust, grok-build)
    /// also uses `acme-lib` in its own build. Both consumers should see
    /// the in-weave `acme-lib` — the config surface is the ONLY surface
    /// that can serve both.
    #[test]
    fn cargo_config_surface_reaches_both_active_member_and_opt_out() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Weave-native lib.
        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        // Active member consuming acme-lib (this drives the derived scan
        // to emit the patch).
        write_file(
            root,
            "github/chatly/server/Cargo.toml",
            "[package]\nname = \"chatly-server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"1.0\"\n",
        );
        // Nested-workspace opt-out. It exists in the weave but is
        // structurally excluded from the workspace manifest — the
        // manifest surface can't patch its builds.
        write_file(
            root,
            "github/xai-org/grok-build/Cargo.toml",
            "[workspace]\nmembers = [\"crates/consumer\"]\nresolver = \"2\"\n",
        );
        write_file(
            root,
            "github/xai-org/grok-build/crates/consumer/Cargo.toml",
            "[package]\nname = \"grok-consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"1.0\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/lib", Role::Owned),
            ("github/chatly/server", Role::Owned),
            ("github/xai-org/grok-build", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("patch = \"derived\"\npatch-surface = \"cargo-config\"\nexclude = [\"github/xai-org/grok-build\"]\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        // Manifest surface is inert.
        let manifest_content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            !manifest_content.contains("[patch"),
            "cargo-config surface must not write into the manifest; got:\n{manifest_content}"
        );

        // Config surface carries the patch.
        let config_path = root.join(".cargo").join("config.toml");
        let config_content = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            config_content.contains("acme-lib"),
            "patch must land in .cargo/config.toml; got:\n{config_content}"
        );
        assert!(
            config_content.contains("\"github/acme/lib\""),
            "path is weave-root-relative (cargo resolves against \
             parent-of-.cargo); got:\n{config_content}"
        );
        // The opt-out repo is present at the tested path. Cargo's upward
        // walk from grok-consumer would land on this file (structural,
        // not asserted via cargo here — see e2e_cargo_test.rs for the
        // cargo-metadata assertion).
        let opt_out_path = root.join("github/xai-org/grok-build/crates/consumer");
        assert!(opt_out_path.exists(), "opt-out repo layout expected");
    }

    /// The `manifest` surface remains the default: an unspecified
    /// `patch-surface` behaves EXACTLY like the pre-2026 output. Zero
    /// migration for existing manifests.
    #[test]
    fn manifest_surface_is_default_zero_migration() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/lib", Role::Owned),
            ("github/acme/app", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        // No patch-surface key — default is manifest.
        let config = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let manifest_content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            manifest_content.contains("[patch.crates-io"),
            "manifest surface default: patch lands in Cargo.toml; got:\n{manifest_content}"
        );
        // No `../` prefix on manifest surface — paths are weave-relative.
        assert!(
            manifest_content.contains("\"github/acme/lib\"")
                || manifest_content.contains("'github/acme/lib'"),
            "manifest surface path stays weave-relative (no `../` prefix); got:\n{manifest_content}"
        );
        // No `.cargo/config.toml` generated under manifest surface.
        assert!(
            !root.join(".cargo").join("config.toml").exists(),
            "manifest surface must NOT generate .cargo/config.toml"
        );
    }

    /// User-authored keys in a pre-existing `.cargo/config.toml`
    /// (linker flags, per-target settings, hand-written unmarked patch
    /// entries) survive activation. Same hybrid ownership as the
    /// manifest surface — the marker is the pen.
    #[test]
    fn cargo_config_surface_preserves_user_authored_keys() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Seed a `.cargo/config.toml` with mixed user content: a linker
        // flag section, a user-authored `[patch.crates-io]` entry (no
        // rwv marker), and a hand-written comment.
        write_file(
            root,
            ".cargo/config.toml",
            r#"# hand-written weave-level policy
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=lld"]

[patch.crates-io]
vendor-foo = { git = "https://user.example.com/vendor-foo.git" }
"#,
        );

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/lib", Role::Owned),
            ("github/acme/app", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config =
            IntegrationConfig::from_toml("patch = \"derived\"\npatch-surface = \"cargo-config\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();

        // User's linker flag survives.
        assert!(
            content.contains("link-arg=-fuse-ld=lld"),
            "user-authored linker flags must survive; got:\n{content}"
        );
        assert!(
            content.contains("[target.x86_64-unknown-linux-gnu]"),
            "user-authored target block must survive; got:\n{content}"
        );
        // User's hand-authored patch entry survives untouched.
        assert!(
            content.contains("https://user.example.com/vendor-foo.git"),
            "user-authored [patch.crates-io].vendor-foo must survive; got:\n{content}"
        );
        // rwv's entry landed next to it, marked.
        assert!(
            content.contains("acme-lib"),
            "rwv-derived patch must land; got:\n{content}"
        );
        assert!(
            content.contains("managed by rwv"),
            "rwv-generated entry must carry marker; got:\n{content}"
        );
    }

    /// Deactivate under the cargo-config surface strips the rwv-marker
    /// entries from `.cargo/config.toml`, preserves user keys, and
    /// deletes the file (and `.cargo/`) iff the strip leaves nothing.
    #[test]
    fn deactivate_strips_marked_entries_from_cargo_config_surface() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Seed the managed Cargo.toml (rwv-marker workspace).
        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
# managed by rwv
members = ["github/acme/app"]
# managed by rwv
resolver = "2"
"#,
        );
        // Seed a `.cargo/config.toml` with mixed content: a marked
        // rwv-generated entry plus a user-authored key. Deactivate must
        // strip the former and preserve the latter.
        write_file(
            root,
            ".cargo/config.toml",
            r#"[patch.crates-io]
# managed by rwv
acme-lib = { path = "../github/acme/lib" }
vendor-foo = { git = "https://user.example.com/vendor-foo.git" }
"#,
        );

        CargoWorkspace.deactivate(root).unwrap();

        let content = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert!(
            !content.contains("acme-lib"),
            "rwv-marker entry must be stripped from cargo-config; got:\n{content}"
        );
        assert!(
            content.contains("vendor-foo"),
            "user-authored entry must survive; got:\n{content}"
        );
    }

    /// Deactivate under the cargo-config surface deletes an emptied
    /// `.cargo/config.toml` and prunes the parent `.cargo/` dir when it
    /// too is empty. Mirrors the file-deletion rule for hybrid files.
    #[test]
    fn deactivate_deletes_emptied_cargo_config_and_dir() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Managed Cargo.toml with only rwv-owned keys.
        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
# managed by rwv
members = ["github/acme/app"]
# managed by rwv
resolver = "2"
"#,
        );
        // Cargo config with ONLY rwv-marker patch entries — after strip,
        // the file is empty.
        write_file(
            root,
            ".cargo/config.toml",
            r#"[patch.crates-io]
# managed by rwv
acme-lib = { path = "../github/acme/lib" }
"#,
        );

        CargoWorkspace.deactivate(root).unwrap();

        assert!(
            !root.join(".cargo").join("config.toml").exists(),
            "emptied cargo-config must be deleted"
        );
        assert!(
            !root.join(".cargo").exists(),
            "empty .cargo/ dir must be pruned"
        );
    }

    /// Deactivate under the cargo-config surface preserves an unrelated
    /// sibling in `.cargo/` (e.g. `credentials`) — the dir prune only
    /// fires when the dir is EMPTY.
    #[test]
    fn deactivate_preserves_dot_cargo_siblings() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
# managed by rwv
members = ["github/acme/app"]
# managed by rwv
resolver = "2"
"#,
        );
        write_file(
            root,
            ".cargo/config.toml",
            r#"[patch.crates-io]
# managed by rwv
acme-lib = { path = "../github/acme/lib" }
"#,
        );
        // User-authored sibling (e.g. cargo credentials).
        write_file(
            root,
            ".cargo/credentials.toml",
            "[registry]\ntoken = \"x\"\n",
        );

        CargoWorkspace.deactivate(root).unwrap();

        // Config gone.
        assert!(
            !root.join(".cargo").join("config.toml").exists(),
            "emptied cargo-config must be deleted"
        );
        // Dir SURVIVES because the sibling exists.
        assert!(
            root.join(".cargo").exists(),
            ".cargo/ must survive when it holds user siblings"
        );
        assert!(
            root.join(".cargo").join("credentials.toml").exists(),
            "user sibling must survive"
        );
    }

    /// The wrapper name every test on this page can rely on being
    /// runnable: the binary running this test suite.
    const PRESENT_WRAPPER: &str = "cargo";
    const ABSENT_WRAPPER: &str = "rwv-test-absent-wrapper";

    fn wrapper_ctx_parts(root: &Path) -> (Manifest, ProjectName) {
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        (
            make_manifest(vec![("github/acme/app", Role::Owned)]),
            ProjectName::new("test-project").unwrap(),
        )
    }

    #[test]
    fn rustc_wrapper_writes_marked_build_key_when_on_path() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        let (manifest, project) = wrapper_ctx_parts(root);
        let config =
            IntegrationConfig::from_toml(&format!("rustc-wrapper = \"{PRESENT_WRAPPER}\"\n"));
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert!(
            content.contains("[build]"),
            "wrapper key must land under [build]; got:\n{content}"
        );
        assert!(
            content.contains(&format!("rustc-wrapper = \"{PRESENT_WRAPPER}\"")),
            "wrapper key must carry the configured name; got:\n{content}"
        );
        assert!(
            content.contains("managed by rwv"),
            "wrapper key must carry the rwv marker; got:\n{content}"
        );
    }

    #[test]
    fn rustc_wrapper_writes_nothing_when_binary_is_absent() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        let (manifest, project) = wrapper_ctx_parts(root);
        let config =
            IntegrationConfig::from_toml(&format!("rustc-wrapper = \"{ABSENT_WRAPPER}\"\n"));
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        assert!(
            !root.join(".cargo").join("config.toml").exists(),
            "no wrapper on PATH must mean no generated .cargo/config.toml — \
             cargo hard-errors on a wrapper it cannot spawn"
        );
    }

    #[test]
    fn rustc_wrapper_strips_marked_key_when_detection_turns_off() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        let (manifest, project) = wrapper_ctx_parts(root);
        let cache = HashMap::new();

        let with =
            IntegrationConfig::from_toml(&format!("rustc-wrapper = \"{PRESENT_WRAPPER}\"\n"));
        let ctx = make_ctx(root, &project, &manifest, &with, &cache);
        CargoWorkspace.activate(&ctx).unwrap();
        assert!(root.join(".cargo").join("config.toml").exists());

        // The uninstall path: detection answers None (here via a name that
        // resolves nowhere), and the marked key — whatever value it holds —
        // must go, taking the emptied file and dir with it.
        let without =
            IntegrationConfig::from_toml(&format!("rustc-wrapper = \"{ABSENT_WRAPPER}\"\n"));
        let ctx = make_ctx(root, &project, &manifest, &without, &cache);
        CargoWorkspace.activate(&ctx).unwrap();

        assert!(
            !root.join(".cargo").join("config.toml").exists(),
            "stripping the only marked key must prune the file"
        );
        assert!(
            !root.join(".cargo").exists(),
            "an emptied .cargo/ dir must be pruned"
        );
    }

    #[test]
    fn rustc_wrapper_defers_to_a_user_authored_key() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        let (manifest, project) = wrapper_ctx_parts(root);
        write_file(
            root,
            ".cargo/config.toml",
            "[build]\nrustc-wrapper = \"mine\"\njobs = 4\n",
        );
        let config =
            IntegrationConfig::from_toml(&format!("rustc-wrapper = \"{PRESENT_WRAPPER}\"\n"));
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert!(
            content.contains("rustc-wrapper = \"mine\""),
            "an unmarked user key holds the pen; got:\n{content}"
        );
        assert!(
            !content.contains("managed by rwv"),
            "deferring must not decorate the user's key; got:\n{content}"
        );
        assert!(
            content.contains("jobs = 4"),
            "unrelated user keys must survive; got:\n{content}"
        );
    }

    #[test]
    fn rustc_wrapper_coexists_with_cargo_config_patches() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );
        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();

        let both = IntegrationConfig::from_toml(&format!(
            "patch = \"derived\"\npatch-surface = \"cargo-config\"\n\
             rustc-wrapper = \"{PRESENT_WRAPPER}\"\n"
        ));
        let ctx = make_ctx(root, &project, &manifest, &both, &cache);
        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert!(content.contains("[patch.crates-io"), "got:\n{content}");
        assert!(
            content.contains(&format!("rustc-wrapper = \"{PRESENT_WRAPPER}\"")),
            "got:\n{content}"
        );

        // Dropping the wrapper must strip only the [build] key — the patch
        // entries and their file survive.
        let patches_only =
            IntegrationConfig::from_toml("patch = \"derived\"\npatch-surface = \"cargo-config\"\n");
        let ctx = make_ctx(root, &project, &manifest, &patches_only, &cache);
        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert!(content.contains("[patch.crates-io"), "got:\n{content}");
        assert!(
            !content.contains("rustc-wrapper"),
            "wrapper key must be stripped; got:\n{content}"
        );
    }

    #[test]
    fn deactivate_strips_the_marked_wrapper_key() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        let (manifest, project) = wrapper_ctx_parts(root);
        let config =
            IntegrationConfig::from_toml(&format!("rustc-wrapper = \"{PRESENT_WRAPPER}\"\n"));
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        CargoWorkspace.activate(&ctx).unwrap();
        assert!(root.join(".cargo").join("config.toml").exists());

        CargoWorkspace.deactivate(root).unwrap();

        assert!(
            !root.join(".cargo").join("config.toml").exists(),
            "deactivate must strip the marked wrapper key and prune the file"
        );
    }

    #[test]
    fn verify_is_clean_after_wrapper_activation_and_drifts_on_a_stale_key() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        let (manifest, project) = wrapper_ctx_parts(root);
        let cache = HashMap::new();

        // The lock is activate_hook's product, which this test does not run.
        write_file(root, "Cargo.lock", "version = 3\n");

        let with =
            IntegrationConfig::from_toml(&format!("rustc-wrapper = \"{PRESENT_WRAPPER}\"\n"));
        let ctx = make_ctx(root, &project, &manifest, &with, &cache);
        CargoWorkspace.activate(&ctx).unwrap();
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "verify must be clean right after activation; got: {issues:?}"
        );

        // Config now expects no wrapper (detection answers None), but the
        // marked key is still on disk — the stale-key drift an operator
        // sees between an uninstall and the next materialization.
        let without =
            IntegrationConfig::from_toml(&format!("rustc-wrapper = \"{ABSENT_WRAPPER}\"\n"));
        let ctx = make_ctx(root, &project, &manifest, &without, &cache);
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.iter().any(|i| i.message.contains("rustc-wrapper")),
            "a marked wrapper key with no detected binary must read as drift; got: {issues:?}"
        );
    }

    /// Structural: `PatchSurface` is an ENUM (not two booleans), so
    /// enabling both surfaces at once is impossible by construction —
    /// a `patch-surface` field can only carry ONE value. This is the
    /// required "double-patch prevention"; it lives at the type
    /// level and is confirmed here by observing that only one surface
    /// ever gets written.
    #[test]
    fn only_one_surface_writes_per_activation() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/lib", Role::Owned),
            ("github/acme/app", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();

        // Round 1: cargo-config surface.
        let config_a =
            IntegrationConfig::from_toml("patch = \"derived\"\npatch-surface = \"cargo-config\"\n");
        let ctx_a = make_ctx(root, &project, &manifest, &config_a, &cache);
        CargoWorkspace.activate(&ctx_a).unwrap();
        let manifest_a = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let config_a_content =
            std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert!(!manifest_a.contains("[patch"), "surface a: manifest inert");
        assert!(
            config_a_content.contains("acme-lib"),
            "surface a: config carries patch"
        );

        // Round 2: manifest surface (fresh fixture — no prior state,
        // because in a real workspace a mode flip is a full re-activation).
        // Delete round 1 output first, matching activate_intent's model.
        CargoWorkspace.deactivate(root).unwrap();
        let config_b = IntegrationConfig::from_toml("patch = \"derived\"\n");
        let ctx_b = make_ctx(root, &project, &manifest, &config_b, &cache);
        CargoWorkspace.activate(&ctx_b).unwrap();
        let manifest_b = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            manifest_b.contains("[patch.crates-io"),
            "surface b: manifest carries patch"
        );
        assert!(
            !root.join(".cargo").join("config.toml").exists(),
            "surface b: config surface not written"
        );
    }

    /// verify() under cargo-config surface classifies MISSING /
    /// USER-HELD / DRIFT / CLEAN for `.cargo/config.toml`.
    #[test]
    fn cargo_config_surface_verify_three_state() {
        use repoweave::integration::Integration;

        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );
        // Managed Cargo.toml must exist (verify_cargo_toml runs first).
        write_file(
            root,
            "Cargo.toml",
            r#"[workspace]
# managed by rwv
members = ["github/acme/app", "github/acme/lib"]
# managed by rwv
resolver = "2"
"#,
        );

        let manifest = make_manifest(vec![
            ("github/acme/lib", Role::Owned),
            ("github/acme/app", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config =
            IntegrationConfig::from_toml("patch = \"derived\"\npatch-surface = \"cargo-config\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // (a) MISSING — no .cargo/config.toml yet, but patch is derived
        // and app depends on lib.
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        let has_missing = issues.iter().any(|i| {
            i.message.replace('\\', "/").contains(".cargo/config.toml")
                && i.message.contains("missing")
        });
        assert!(
            has_missing,
            "expected MISSING finding for .cargo/config.toml; got: {issues:?}"
        );

        // (b) DRIFT — a stale marked entry for a different crate.
        write_file(
            root,
            ".cargo/config.toml",
            r#"[patch.crates-io]
# managed by rwv
stale-crate = { path = "../github/nowhere" }
"#,
        );
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        let has_drift = issues
            .iter()
            .any(|i| i.message.contains(".cargo/config.toml") && i.message.contains("drift"));
        assert!(
            has_drift,
            "expected DRIFT finding for stale marked entry; got: {issues:?}"
        );

        // (c) USER-HELD — same expected crate key but WITHOUT the marker.
        write_file(
            root,
            ".cargo/config.toml",
            r#"[patch.crates-io]
acme-lib = { path = "../github/acme/lib" }
"#,
        );
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        let has_user_held = issues.iter().any(|i| {
            i.message.replace('\\', "/").contains(".cargo/config.toml")
                && i.message.contains("unmarked")
        });
        assert!(
            has_user_held,
            "expected USER-HELD finding for unmarked expected key; got: {issues:?}"
        );

        // (d) CLEAN — expected entry present with the marker.
        write_file(
            root,
            ".cargo/config.toml",
            r#"[patch.crates-io]
# managed by rwv
acme-lib = { path = "../github/acme/lib" }
"#,
        );
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        let has_config_finding = issues
            .iter()
            .any(|i| i.message.contains(".cargo/config.toml"));
        assert!(
            !has_config_finding,
            "expected no cargo-config finding when marked entry matches expected; got: {issues:?}"
        );
    }

    /// A member's own `.cargo/config.toml` shadowing the same
    /// `[patch.crates-io].<crate>` key silently defeats the weave-level
    /// entry under the config surface — the same shadowing as the manifest
    /// surface. The scan is target-agnostic (checks all discoverable
    /// member configs regardless of where the weave writes).
    #[test]
    fn cargo_config_surface_shadowing_warning() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "github/acme/lib/Cargo.toml",
            "[package]\nname = \"acme-lib\"\nversion = \"0.3.5\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "github/acme/app/Cargo.toml",
            "[package]\nname = \"acme-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             [dependencies]\nacme-lib = \"0.3\"\n",
        );
        // Shadowing config in the member's tree.
        write_file(
            root,
            "github/acme/app/.cargo/config.toml",
            "[patch.crates-io]\nacme-lib = { path = \"/wrong/place\" }\n",
        );

        let manifest = make_manifest(vec![
            ("github/acme/app", Role::Owned),
            ("github/acme/lib", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config =
            IntegrationConfig::from_toml("patch = \"derived\"\npatch-surface = \"cargo-config\"\n");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Must not panic; write still lands. The warning is advisory
        // (captured via eprintln — not asserted here, mirroring the
        // manifest-surface counterpart which is unit-covered in
        // scan_patch_shadowing_against_keys).
        CargoWorkspace.activate(&ctx).unwrap();

        let config_content =
            std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert!(
            config_content.contains("acme-lib"),
            "output must land even when a member shadows the key; got:\n{config_content}"
        );
    }
}

// ===========================================================================
// cargo-workspace doctor-acceptance battery
// ===========================================================================
//
// Verify() + doctor --fix acceptance tests for cargo-workspace.
// Named `s7_cargo_doctor_*` per the spec so they are discoverable as
// a battery: `cargo test --test integrations_test s7_cargo_doctor_`.
//
// These tests drive the integration directly (verify() / activate()) rather
// than the full CLI doctor path — that is the C17-aligned style.

mod s7_cargo_doctor {
    use super::*;
    use repoweave::integration::Issue;
    use repoweave::integrations::CargoWorkspace;

    /// Filter verify() output to hybrid-Cargo.toml findings only.
    ///
    /// The older tests in this module were written when `verify()` only
    /// inspected the hybrid `Cargo.toml`; it was later extended to also
    /// inspect the fully-owned `Cargo.lock`. To keep those
    /// pre-existing tests focused on their original semantic axis
    /// (Cargo.toml states) without seeding an unrelated Cargo.lock in each
    /// fixture, this helper filters out Cargo.lock findings. The fully-owned
    /// axis is covered separately, by the fully-owned `Cargo.lock` battery below.
    fn cargo_toml_issues(issues: Vec<Issue>) -> Vec<Issue> {
        issues
            .into_iter()
            .filter(|i| !i.message.contains("Cargo.lock"))
            .collect()
    }

    // -----------------------------------------------------------------------
    // MISSING: verify() reports MISSING when Cargo.toml is absent
    // -----------------------------------------------------------------------

    /// Given: cargo-workspace config with members.include = [a, b, c],
    ///        Cargo.toml ABSENT.
    /// Then:  verify() reports a single MISSING+safe_to_fix finding.
    #[test]
    fn s7_cargo_doctor_missing_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Repo "github/cwalv/rvtty" with sub-packages; no root Cargo.toml.
        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\", \"common\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one MISSING issue (Cargo.toml axis), got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
        assert!(
            issue.message.contains("missing"),
            "MISSING issue message should contain 'missing': {}",
            issue.message
        );
        assert!(
            issue.message.contains("rwv doctor --fix"),
            "MISSING issue message should mention 'rwv doctor --fix': {}",
            issue.message
        );
    }

    /// Given: MISSING Cargo.toml.
    /// When:  activate() runs (simulating doctor --fix).
    /// Then:
    ///   - Cargo.toml created with `# managed by rwv` markers
    ///   - members lists rvtty/daemon, rvtty/client, rvtty/common (alphabetical)
    ///   - resolver = "2"
    ///   - Subsequent verify() returns no issues (CLEAN).
    #[test]
    fn s7_cargo_doctor_missing_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\", \"common\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: MISSING.
        let pre_issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert_eq!(pre_issues.len(), 1, "expected MISSING pre-condition");
        assert!(pre_issues[0].safe_to_fix);

        // Simulate doctor --fix: call activate().
        CargoWorkspace.activate(&ctx).unwrap();

        // Cargo.toml must exist now.
        let cargo_toml_path = root.join("Cargo.toml");
        assert!(
            cargo_toml_path.exists(),
            "Cargo.toml must be created after activate"
        );

        let content = std::fs::read_to_string(&cargo_toml_path).unwrap();

        // Markers must be present.
        assert!(
            content.contains("# managed by rwv"),
            "Cargo.toml must have '# managed by rwv' markers after activate: {content}"
        );

        // Members must be sorted alphabetically: client < common < daemon.
        assert!(
            content.contains("\"github/cwalv/rvtty/client\""),
            "members must include rvtty/client: {content}"
        );
        assert!(
            content.contains("\"github/cwalv/rvtty/common\""),
            "members must include rvtty/common: {content}"
        );
        assert!(
            content.contains("\"github/cwalv/rvtty/daemon\""),
            "members must include rvtty/daemon: {content}"
        );

        // Check alphabetical order in the raw text.
        let client_pos = content.find("rvtty/client").unwrap();
        let common_pos = content.find("rvtty/common").unwrap();
        let daemon_pos = content.find("rvtty/daemon").unwrap();
        assert!(
            client_pos < common_pos && common_pos < daemon_pos,
            "members must be alphabetically sorted: client < common < daemon"
        );

        // resolver = "2".
        assert!(
            content.contains("resolver = \"2\""),
            "Cargo.toml must set resolver = \"2\": {content}"
        );

        // Post-condition: CLEAN (no verify issues on the Cargo.toml axis).
        // Cargo.lock is still absent (activate() does not run the hook), so
        // the fully-owned arm would emit MISSING — exercised by the
        // fully-owned `Cargo.lock` battery, and out of scope here.
        let post_issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert!(
            post_issues.is_empty(),
            "verify() must return no Cargo.toml issues after activate (CLEAN), got: {post_issues:?}"
        );
    }

    // -----------------------------------------------------------------------
    // DRIFT: verify() reports DRIFT when markers are present but
    //      on-disk content doesn't match config
    // -----------------------------------------------------------------------

    /// Given: Cargo.toml exists with rwv markers but outdated members list.
    /// Then:  verify() reports a single DRIFT+safe_to_fix finding.
    #[test]
    fn s7_cargo_doctor_drift_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Write a Cargo.toml with per-key rwv markers but only one member (outdated).
        // Marker format: `# managed by rwv` as a prefix decoration on each owned key,
        // matching what TomlDoc's merge_activate produces.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n# managed by rwv\nresolver = \"2\"\n",
        );

        let config = IntegrationConfig::from_toml(
            // Config now has two members (drift: common was added to config but not file).
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"common\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one DRIFT issue (Cargo.toml axis), got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
        assert!(
            issue.message.contains("drift"),
            "DRIFT issue message should contain 'drift': {}",
            issue.message
        );
    }

    /// Given: DRIFT Cargo.toml.
    /// When:  activate() runs.
    /// Then:  verify() returns no issues (CLEAN).
    #[test]
    fn s7_cargo_doctor_drift_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Seed: only daemon in members (drift), with per-key markers.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n# managed by rwv\nresolver = \"2\"\n",
        );

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"common\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: DRIFT.
        let pre_issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert_eq!(pre_issues.len(), 1, "expected DRIFT pre-condition");

        // Simulate fix.
        CargoWorkspace.activate(&ctx).unwrap();

        // Post-condition: CLEAN on the Cargo.toml axis.
        let post_issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert!(
            post_issues.is_empty(),
            "verify() must return no Cargo.toml issues after activate (CLEAN), got: {post_issues:?}"
        );

        // Confirm common is now in the file.
        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("\"github/cwalv/rvtty/common\""),
            "common must be in members after fix: {content}"
        );
    }

    // -----------------------------------------------------------------------
    // USER-HELD: verify() reports USER-HELD, doctor --fix is a no-op
    // -----------------------------------------------------------------------

    /// Given: Cargo.toml exists with [workspace] members/resolver, NO markers.
    /// Then:  verify() reports a single USER-HELD+!safe_to_fix finding.
    #[test]
    fn s7_cargo_doctor_user_held_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No "# managed by rwv" marker — user holds the pen.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"github/cwalv/rvtty/daemon\"]\nresolver = \"2\"\n",
        );

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue (Cargo.toml axis), got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            !issue.safe_to_fix,
            "USER-HELD issue must NOT be safe_to_fix (safe_to_fix=false)"
        );
        assert!(
            issue.message.contains("NOT auto-take-over") || issue.message.contains("not auto"),
            "USER-HELD issue must describe no-takeover: {}",
            issue.message
        );
    }

    /// Given: USER-HELD Cargo.toml (no markers).
    /// When:  activate() runs (simulating what doctor --fix would call if safe_to_fix
    ///        were true — but it won't, so this tests the merge's own guard).
    /// Then:  The file is UNCHANGED (merge_activate's verify-and-warn semantics
    ///        protect the user-held keys).
    #[test]
    fn s7_cargo_doctor_user_held_file_unchanged_after_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let original_content =
            "[workspace]\nmembers = [\"github/cwalv/rvtty/daemon\"]\nresolver = \"2\"\n";
        write_file(root, "Cargo.toml", original_content);

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"common\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Verify reports USER-HELD with safe_to_fix=false.
        let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert_eq!(issues.len(), 1);
        assert!(
            !issues[0].safe_to_fix,
            "must be USER-HELD (not safe_to_fix)"
        );

        // Even if activate() is called (guard: doctor --fix does NOT call it
        // for user-held issues; this test verifies the merge's own protection),
        // the [workspace] content is left intact (merge defers to user).
        CargoWorkspace.activate(&ctx).unwrap();

        let after_content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            !after_content.contains("# managed by rwv"),
            "user-held file must NOT have rwv markers added by activate: {after_content}"
        );
        // The user's original members list is preserved (common was NOT added).
        assert!(
            !after_content.contains("rvtty/common"),
            "user-held members must not be modified by activate: {after_content}"
        );
    }

    // -----------------------------------------------------------------------
    // CLEAN: verify() returns no issues when file is up to date
    // -----------------------------------------------------------------------

    /// Given: Cargo.toml was written by activate() (markers present, content
    ///        matches config).
    /// Then:  verify() returns no issues (CLEAN).
    #[test]
    fn s7_cargo_doctor_clean_after_fresh_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert!(
            issues.is_empty(),
            "verify() must return no Cargo.toml issues for a freshly-activated Cargo.toml, got: {issues:?}"
        );
    }

    // -----------------------------------------------------------------------
    // resolver DefaultOnly
    // -----------------------------------------------------------------------

    /// Greenfield: empty Cargo.toml gets `resolver = "2"` set by activate().
    ///
    /// Given: fresh empty Cargo.toml (or no file at all).
    /// When:  activate() runs.
    /// Then:  resolver = "2" appears in the file.
    #[test]
    fn resolver_default_only_greenfield_sets_resolver_2() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/cwalv/myrepo/Cargo.toml");

        let manifest = make_manifest(vec![("github/cwalv/myrepo", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("resolver = \"2\""),
            "greenfield activate must write resolver = \"2\": {content}"
        );
    }

    /// Existing without resolver: file with marker + no resolver key →
    /// DefaultOnly sets "2".
    ///
    /// Given: Cargo.toml with managed marker on members but no resolver key.
    /// When:  activate() runs.
    /// Then:  resolver = "2" is added to the file.
    #[test]
    fn resolver_default_only_no_resolver_key_sets_resolver_2() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // File has marker+members but no resolver.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n",
        );
        touch(root, "github/cwalv/rvtty/daemon/Cargo.toml");

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("resolver = \"2\""),
            "activate must write resolver = \"2\" when key is absent: {content}"
        );
    }

    /// Operator override: existing Cargo.toml with marker + `resolver = "1"` →
    /// after activate, resolver still "1" (DefaultOnly does not overwrite).
    ///
    /// Given: Cargo.toml with managed markers AND resolver = "1" (compat setting).
    /// When:  activate() runs.
    /// Then:  resolver is still "1" in the file (not overwritten to "2").
    #[test]
    fn resolver_default_only_operator_override_preserved() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Cargo.toml seeded with resolver = "1" and the managed marker.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n\
             # managed by rwv\nresolver = \"1\"\n",
        );
        touch(root, "github/cwalv/rvtty/daemon/Cargo.toml");

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("resolver = \"1\""),
            "activate must NOT overwrite operator's resolver = \"1\": {content}"
        );
        assert!(
            !content.contains("resolver = \"2\""),
            "resolver must not be changed to \"2\" when operator set \"1\": {content}"
        );
    }

    /// Resolver drift is CLEAN: file with marker + resolver = "1" → verify()
    /// returns no issues (DefaultOnly drift is always CLEAN).
    ///
    /// Given: Cargo.toml with managed markers and members matching config,
    ///        but resolver = "1" (differs from rwv's default "2").
    /// Then:  verify() returns no issues (CLEAN — resolver drift is not a
    ///        DRIFT finding for DefaultOnly keys).
    #[test]
    fn resolver_default_only_drift_is_clean() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/cwalv/rvtty/daemon/Cargo.toml");

        // members matches config; resolver deviates from default "2".
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n\
             # managed by rwv\nresolver = \"1\"\n",
        );

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert!(
            issues.is_empty(),
            "resolver drift (DefaultOnly) must be CLEAN on Cargo.toml axis — got: {issues:?}"
        );
    }

    /// Members still drift: file with marker + correct resolver but wrong members
    /// → DRIFT finding (members is still Author).
    ///
    /// Given: Cargo.toml with managed markers, resolver = "2", but members
    ///        does not match config (drift on members, not resolver).
    /// Then:  verify() reports exactly one DRIFT issue.
    #[test]
    fn resolver_default_only_members_drift_still_reported() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // members is stale (only daemon), config expects daemon + client.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n\
             # managed by rwv\nresolver = \"2\"\n",
        );

        let config = IntegrationConfig::from_toml(
            "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\"]\n",
        );
        let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
        assert_eq!(
            issues.len(),
            1,
            "members drift must still produce a DRIFT issue on Cargo.toml axis, got: {issues:?}"
        );
        assert!(issues[0].safe_to_fix, "DRIFT issue must be safe_to_fix");
        assert!(
            issues[0].message.contains("drift"),
            "DRIFT issue message should contain 'drift': {}",
            issues[0].message
        );
    }

    // -----------------------------------------------------------------------
    // Fully-owned Cargo.lock verify
    //
    // The three-state verify() shape (MISSING / DRIFT / USER-HELD) was
    // originally hybrid-only; USER-HELD requires an owned-key + marker pair
    // that fully-owned files don't have. This battery covers the
    // fully-owned axis on `Cargo.lock`:
    //
    //   - MISSING (file absent when generation expected) → DRIFT, safe_to_fix
    //   - Parse-fail (garbage bytes / cargo half-write) → DRIFT, safe_to_fix
    //   - Present + parseable → CLEAN
    //
    // Anchors the audit finding: previously `verify()` ignored Cargo.lock
    // entirely — any mutation was invisible to doctor.
    // -----------------------------------------------------------------------

    /// Helper: build a fixture where cargo-workspace has active work
    /// (`Cargo.toml` present, marker+members correct) so verify() reaches the
    /// Cargo.lock arm without short-circuiting on the hybrid arm.
    ///
    /// Returns (tempdir, project, manifest, config, cache) to keep the borrow
    /// pattern the other tests use.
    fn s7_6_fixture() -> (
        TempDir,
        ProjectName,
        Manifest,
        IntegrationConfig,
        HashMap<String, Vec<String>>,
    ) {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Repo with a Cargo.toml so `has_active_cargo_work` returns true.
        touch(root, "github/cwalv/mylib/Cargo.toml");

        // Write a clean, marker-decorated root Cargo.toml matching the config
        // so the hybrid Cargo.toml arm of verify() is CLEAN.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/mylib\"]\n\
             # managed by rwv\nresolver = \"2\"\n",
        );

        let config = IntegrationConfig::default();
        let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();

        (tmp, project, manifest, config, cache)
    }

    /// Given: Cargo.toml is CLEAN (marker + matching members), Cargo.lock
    ///        is ABSENT.
    /// Then:  verify() reports a MISSING finding for Cargo.lock naming the
    ///        file, the state, and the `rwv doctor --fix` repair verb.
    ///
    /// Regression: pre-fix, doctor exited 0 with no report even when
    /// the fully-owned lockfile was gone.
    #[test]
    fn s7_6_cargo_lock_missing_reports_drift_naming_doctor_fix() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Confirm Cargo.lock is absent (fixture doesn't create it).
        assert!(!root.join("Cargo.lock").exists());

        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one MISSING finding for Cargo.lock, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            issue.safe_to_fix,
            "MISSING Cargo.lock must be safe_to_fix (doctor --fix regenerates)"
        );
        // Message must name the file (house pattern: name the file).
        assert!(
            issue.message.contains("Cargo.lock"),
            "MISSING message must name the file: {}",
            issue.message
        );
        // Message must name the state (house pattern: name the state).
        assert!(
            issue.message.contains("missing"),
            "MISSING message must name the state ('missing'): {}",
            issue.message
        );
        // Message must name the repair verb (house pattern: name the repair).
        assert!(
            issue.message.contains("rwv doctor --fix"),
            "MISSING message must name `rwv doctor --fix`: {}",
            issue.message
        );
    }

    /// Given: Cargo.lock present but not valid TOML (out-of-band mutation
    ///        or interrupted cargo write leaves garbage bytes).
    /// Then:  verify() reports a DRIFT finding naming Cargo.lock, "drift",
    ///        and the `rwv doctor --fix` repair verb.
    #[test]
    fn s7_6_cargo_lock_corrupt_reports_drift_naming_doctor_fix() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();

        // Write garbage bytes — not a valid TOML document.
        write_file(root, "Cargo.lock", "this is not toml \x00 [[[");

        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one DRIFT finding for corrupt Cargo.lock, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            issue.safe_to_fix,
            "corrupt Cargo.lock is safe_to_fix (doctor --fix regenerates)"
        );
        assert!(
            issue.message.contains("Cargo.lock"),
            "DRIFT message must name the file: {}",
            issue.message
        );
        assert!(
            issue.message.contains("drift"),
            "DRIFT message must name the state ('drift'): {}",
            issue.message
        );
        assert!(
            issue.message.contains("rwv doctor --fix"),
            "DRIFT message must name `rwv doctor --fix`: {}",
            issue.message
        );
    }

    /// Given: Cargo.lock present and parseable as TOML.
    /// Then:  verify() reports no Cargo.lock finding (CLEAN).
    ///
    /// This anchors the intentional scope bound: deep content-drift (cargo
    /// silently rewrote pinned versions) is NOT detected without running
    /// cargo. Present-and-parseable is CLEAN.
    #[test]
    fn s7_6_cargo_lock_present_and_parseable_is_clean() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();

        // Minimal valid Cargo.lock shape (top-level version + empty package
        // array is enough for the parse-only check).
        write_file(
            root,
            "Cargo.lock",
            "version = 3\n\n[[package]]\nname = \"mylib\"\nversion = \"0.1.0\"\n",
        );

        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "present + parseable Cargo.lock must be CLEAN, got: {issues:?}"
        );
    }

    /// Regression: fully-owned Cargo.lock MUST NOT be reported as USER-HELD
    /// even in the pathological case where a file is present without markers.
    /// USER-HELD is a hybrid-marker concept and does not apply to fully-owned
    /// files.
    #[test]
    fn s7_6_cargo_lock_never_reports_user_held() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();

        // A "user-authored" Cargo.lock analog: valid TOML, no rwv marker.
        // Fully-owned semantics say this is CLEAN, not USER-HELD.
        write_file(root, "Cargo.lock", "version = 3\n");

        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        let issues = CargoWorkspace.verify(&ctx).unwrap();

        // No issue at all. If there were an issue, it must NOT be
        // safe_to_fix=false (the USER-HELD signature).
        for issue in &issues {
            assert!(
                issue.safe_to_fix,
                "fully-owned Cargo.lock must never emit a USER-HELD (safe_to_fix=false) issue: {issue:?}"
            );
        }
        assert!(
            issues.is_empty(),
            "present+parseable fully-owned file must be CLEAN, got: {issues:?}"
        );
    }

    /// Regression: hybrid Cargo.toml USER-HELD detection must survive the
    /// verify() split (Cargo.toml first, Cargo.lock second). A Cargo.toml
    /// with unmarked members + present Cargo.lock still emits exactly one
    /// USER-HELD finding for the hybrid file — the fully-owned arm stays
    /// CLEAN when Cargo.lock is present-and-parseable.
    #[test]
    fn s7_6_hybrid_user_held_unchanged_by_fully_owned_split() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No "# managed by rwv" marker — user holds the pen on Cargo.toml.
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"github/cwalv/mylib\"]\nresolver = \"2\"\n",
        );
        touch(root, "github/cwalv/mylib/Cargo.toml");
        // Cargo.lock is present + parseable so the fully-owned arm is CLEAN.
        write_file(root, "Cargo.lock", "version = 3\n");

        let config =
            IntegrationConfig::from_toml("[members.\"github/cwalv/mylib\"]\ninclude = [\".\"]\n");
        let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue for Cargo.toml, got: {issues:?}"
        );
        assert!(
            !issues[0].safe_to_fix,
            "USER-HELD hybrid finding must NOT be safe_to_fix, got: {:?}",
            issues[0]
        );
    }

    // -----------------------------------------------------------------------
    // C3 regeneration gap: activate_hook refuses cleanly when the
    //      managed file is missing, naming `rwv doctor --fix`.
    //
    // Empirical evidence from the audit: a repo that acquired its
    // Cargo.toml AFTER `rwv add` never had its managed Cargo.toml generated,
    // so `activate` blew up in the activate_hook (cargo generate-lockfile
    // has no root manifest to lock against) with the confusing "workspace
    // may be partially activated" wrap. This battery pins the FALLBACK
    // branch of the design's either/or: activate is a context verb
    // and must not author — activate_hook precheck bails with a clear
    // message pointing to the intent-mode recovery verb.
    // -----------------------------------------------------------------------

    /// Given: cargo-workspace has active work but the managed Cargo.toml
    ///        was never generated (the "acquired manifest after add" gap).
    /// When:  activate_hook runs.
    /// Then:  it bails with a clear error naming `rwv doctor --fix`
    ///        BEFORE running cargo (which would fail with a confusing wrap).
    #[test]
    fn s7_7_activate_hook_refuses_when_managed_file_missing() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Repo has a Cargo.toml so has_active_cargo_work=true, but the
        // ROOT (output_dir) Cargo.toml is absent — the C3 gap shape.
        touch(root, "github/cwalv/mylib/Cargo.toml");
        assert!(!root.join("Cargo.toml").exists());

        let config = IntegrationConfig::default();
        let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let err = CargoWorkspace
            .activate_hook(&ctx)
            .expect_err("activate_hook must refuse when managed file is missing");
        let msg = format!("{err:#}");

        // Names the file.
        assert!(
            msg.contains("Cargo.toml"),
            "error must name the missing managed file: {msg}"
        );
        // Names the recovery verb — the reason we bail early is to give
        // ONE actionable message instead of the "partially activated" wrap.
        assert!(
            msg.contains("rwv doctor --fix"),
            "error must name the recovery verb `rwv doctor --fix`: {msg}"
        );
    }

    /// Given: managed Cargo.toml is missing.
    /// When:  `activate_intent` (the intent-mode write path that
    ///        `rwv doctor --fix` invokes) runs.
    /// Then:  Cargo.toml is authored — the intent path self-heals the C3
    ///        gap, closing the loop that `verify()` opens.
    ///
    /// This is the DOCTOR-FIX-REPAIRS-IT half of the pair — the activate
    /// (context) path refuses cleanly (previous test), and the doctor
    /// (intent) path repairs.
    #[test]
    fn s7_7_activate_intent_regenerates_missing_managed_file() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/cwalv/mylib/Cargo.toml");
        assert!(!root.join("Cargo.toml").exists());

        let config = IntegrationConfig::default();
        let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Intent mode (what doctor --fix invokes) DOES author.
        CargoWorkspace.activate(&ctx).unwrap();

        assert!(
            root.join("Cargo.toml").exists(),
            "activate() (intent mode) must regenerate the missing managed file"
        );
        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            content.contains("# managed by rwv"),
            "regenerated Cargo.toml must carry the rwv marker: {content}"
        );

        // And verify() must now be CLEAN on the hybrid arm.
        let post_issues = CargoWorkspace.verify(&ctx).unwrap();
        // Cargo.lock is still absent (activate() doesn't run the hook), so
        // exactly ONE MISSING finding for Cargo.lock — but the Cargo.toml
        // hybrid arm is CLEAN.
        assert_eq!(
            post_issues.len(),
            1,
            "post-regeneration verify() must have exactly one issue \
             (fully-owned Cargo.lock still MISSING; hybrid Cargo.toml CLEAN), \
             got: {post_issues:?}"
        );
        assert!(
            post_issues[0].message.contains("Cargo.lock"),
            "remaining issue must be for Cargo.lock, got: {}",
            post_issues[0].message
        );
    }

    // -----------------------------------------------------------------------
    // Recorded-digest verify: cargo rewriting Cargo.lock as VALID TOML
    //      (invisible to the parse check).
    //
    // rwv cannot recompute lock content (cargo generate-lockfile output
    // depends on registry state), so the activation hook stamps a SHA-256 of
    // each accepted generation into `.rwv-owned-digests` (output_dir) and
    // verify() compares. Report-not-mandate: WARNING severity,
    // safe_to_fix=false, both exits named. Pre-upgrade workspaces (no digest
    // state) skip the axis silently.
    //
    // These tests stamp via the same helper the hook calls
    // (stamp_owned_digest) — the hook itself needs a real cargo run and is
    // covered by the e2e battery in e2e_cargo_test.rs.
    // -----------------------------------------------------------------------

    use repoweave::owned_state::stamp_owned_digest;

    /// The regression test proper.
    ///
    /// Given: Cargo.lock stamped at generation, then rewritten out-of-band
    ///        as DIFFERENT but VALID TOML (what a cargo invocation does).
    /// Then:  verify() reports a WARNING naming the file, the state
    ///        ("differs from the last rwv-accepted generation"), and BOTH
    ///        consents, spelled as they are invoked. NOT safe_to_fix — the
    ///        operator chooses, and each named verb runs in a workweave,
    ///        which is where this finding is most often read.
    #[test]
    fn s7_8_cargo_rewrite_valid_toml_reports_warning_with_both_exits() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();

        // The generation rwv accepted (simulating the activation hook's
        // stamp — the hook itself needs real cargo; e2e covers it).
        let accepted = "version = 3\n\n[[package]]\nname = \"mylib\"\nversion = \"0.1.0\"\n";
        write_file(root, "Cargo.lock", accepted);
        stamp_owned_digest(root, "Cargo.lock", accepted.as_bytes()).unwrap();

        // Out-of-band cargo rewrite: still perfectly valid TOML — the parse
        // check CANNOT see this. (A dep version was bumped.)
        let rewritten = "version = 3\n\n[[package]]\nname = \"mylib\"\nversion = \"0.2.0\"\n";
        write_file(root, "Cargo.lock", rewritten);

        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one digest-mismatch finding, got: {issues:?}"
        );
        let issue = &issues[0];
        assert_eq!(
            issue.severity,
            Severity::Warning,
            "report-not-mandate: warning severity keeps doctor exit semantics unchanged"
        );
        assert!(
            !issue.safe_to_fix,
            "digest mismatch must NOT be auto-fixed — the operator chooses an exit: {issue:?}"
        );
        // House pattern: name the file.
        assert!(
            issue.message.contains("Cargo.lock"),
            "must name the file: {}",
            issue.message
        );
        // Name the state.
        assert!(
            issue
                .message
                .contains("differs from the last rwv-accepted generation"),
            "must name the state: {}",
            issue.message
        );
        // Name BOTH consents, spelled as they are invoked. A remedy the
        // operator cannot run in the checkout the finding printed in is a dead
        // end, and `rwv activate` — what this used to name — is refused in a
        // workweave.
        assert!(
            issue.message.contains("rwv materialize --adopt-drifted"),
            "must name the adopt exit: {}",
            issue.message
        );
        assert!(
            issue
                .message
                .contains("rwv materialize --regenerate-drifted"),
            "must name the regenerate exit: {}",
            issue.message
        );
        assert!(
            issue.message.contains("restore the file"),
            "must name the restore exit: {}",
            issue.message
        );
        assert!(
            !issue.message.contains("rwv activate"),
            "naming a verb the workweave refuses is the defect this fixed: {}",
            issue.message
        );
    }

    /// Given: digest mismatch (previous test's shape).
    /// When:  activation re-runs and re-stamps (the ACCEPT exit — simulated
    ///        via the same stamp helper the hook calls).
    /// Then:  verify() is clean.
    #[test]
    fn s7_8_reactivation_restamp_returns_clean() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();

        write_file(root, "Cargo.lock", "version = 3\n");
        stamp_owned_digest(root, "Cargo.lock", b"version = 3\n").unwrap();

        // Out-of-band rewrite → mismatch.
        let rewritten = "version = 4\n";
        write_file(root, "Cargo.lock", rewritten);
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        assert_eq!(
            CargoWorkspace.verify(&ctx).unwrap().len(),
            1,
            "precondition: mismatch must be reported"
        );

        // ACCEPT exit: re-activation re-runs the hook, which re-stamps the
        // now-current content.
        stamp_owned_digest(root, "Cargo.lock", rewritten.as_bytes()).unwrap();

        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "re-stamp must accept the new content (clean), got: {issues:?}"
        );
    }

    /// Given: digest mismatch.
    /// When:  the operator takes the RESTORE exit (puts the recorded content
    ///        back, e.g. via VCS).
    /// Then:  verify() is clean — without any re-stamp.
    #[test]
    fn s7_8_restore_exit_returns_clean_without_restamp() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();

        let accepted = "version = 3\n";
        write_file(root, "Cargo.lock", accepted);
        stamp_owned_digest(root, "Cargo.lock", accepted.as_bytes()).unwrap();
        write_file(root, "Cargo.lock", "version = 4\n");

        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        assert_eq!(
            CargoWorkspace.verify(&ctx).unwrap().len(),
            1,
            "precondition: mismatch must be reported"
        );

        // RESTORE exit: put the accepted bytes back.
        write_file(root, "Cargo.lock", accepted);

        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "restoring the recorded content must be clean without re-stamp, got: {issues:?}"
        );
    }

    /// Backward compat: a pre-upgrade workspace has a generated Cargo.lock
    /// but NO digest state. The axis is skipped silently — present +
    /// parseable stays CLEAN, exactly the pre-digest behavior.
    #[test]
    fn s7_8_no_digest_state_skips_axis_silently() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();

        // Any valid-TOML content; no .rwv-owned-digests anywhere.
        write_file(root, "Cargo.lock", "version = 3\n");
        assert!(!root.join(".rwv-owned-digests").exists());

        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "no digest state must skip the axis silently (backward compat), got: {issues:?}"
        );
    }

    /// Digest state must survive doctor --fix of OTHER issues untouched.
    ///
    /// Given: stamped Cargo.lock (digest matches) + a Cargo.toml DRIFT
    ///        (stale members under markers — a safe_to_fix issue).
    /// When:  the doctor --fix write path repairs the Cargo.toml drift
    ///        (unit-level: activate(), which authors the hybrid file but
    ///        does not run hooks).
    /// Then:  `.rwv-owned-digests` is byte-identical, and verify() is fully
    ///        clean (toml repaired; lock digest still matches).
    #[test]
    fn s7_8_digest_state_survives_fix_of_other_issues() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/cwalv/mylib/Cargo.toml");

        // Cargo.toml with markers but STALE members (drift: config expects
        // mylib, file names a repo that no longer exists).
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/oldlib\"]\n\
             # managed by rwv\nresolver = \"2\"\n",
        );

        // Stamped, matching Cargo.lock.
        let lock = "version = 3\n";
        write_file(root, "Cargo.lock", lock);
        stamp_owned_digest(root, "Cargo.lock", lock.as_bytes()).unwrap();
        let digest_before = std::fs::read_to_string(root.join(".rwv-owned-digests")).unwrap();

        let config = IntegrationConfig::default();
        let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Precondition: exactly one issue, and it is the Cargo.toml drift
        // (safe_to_fix) — the lock axis is clean.
        let pre = CargoWorkspace.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "precondition: only the toml drift: {pre:?}");
        assert!(pre[0].safe_to_fix && !pre[0].message.contains("Cargo.lock"));

        // doctor --fix write path for the OTHER issue.
        CargoWorkspace.activate(&ctx).unwrap();

        // Digest state untouched.
        let digest_after = std::fs::read_to_string(root.join(".rwv-owned-digests")).unwrap();
        assert_eq!(
            digest_before, digest_after,
            "fixing an unrelated issue must not touch the digest state"
        );

        // And everything is now clean.
        let post = CargoWorkspace.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "toml repaired + lock digest still matching must be clean, got: {post:?}"
        );
    }

    /// Adversarial: parse-fail beats digest-compare. If the out-of-band
    /// mutation left the lock UNPARSEABLE, the finding is the parse-fail
    /// DRIFT (safe_to_fix=true — regeneration is the only sane exit), not a
    /// digest mismatch on garbage bytes.
    #[test]
    fn s7_8_unparseable_mutation_reports_parse_fail_not_digest_mismatch() {
        let (tmp, project, manifest, config, cache) = s7_6_fixture();
        let root = tmp.path();

        write_file(root, "Cargo.lock", "version = 3\n");
        stamp_owned_digest(root, "Cargo.lock", b"version = 3\n").unwrap();
        // Mutation produced garbage, not valid TOML.
        write_file(root, "Cargo.lock", "half a write [[[");

        let ctx = make_ctx(root, &project, &manifest, &config, &cache);
        let issues = CargoWorkspace.verify(&ctx).unwrap();
        assert_eq!(issues.len(), 1, "exactly one finding, got: {issues:?}");
        assert!(
            issues[0].safe_to_fix,
            "parse-fail must win (regeneration is the exit): {issues:?}"
        );
        assert!(
            issues[0].message.contains("rwv doctor --fix"),
            "parse-fail names the regeneration verb: {}",
            issues[0].message
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // gita uses all repos, not just those with a specific manifest file
        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Owned),
            ("github/chatly/web", Role::Owned),
            ("github/chatly/protocol", Role::Fork),
        ]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Owned),
            ("github/chatly/web", Role::Owned),
            ("github/chatly/protocol", Role::Fork),
        ]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
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
        let workspace_tmp = common::tempdir().unwrap();
        let workspace_root = workspace_tmp.path();
        let weave_tmp = common::tempdir().unwrap();
        let output_dir = weave_tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir,
            workspace_root,
            container_kind: ContainerKind::Primary,
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

    #[cfg(unix)]
    #[test]
    fn check_warns_when_gita_not_on_path() {
        let absent = doctor_json_on_tool_only_path(
            "gita",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            &[],
        );
        assert!(
            reports_tool_missing(&absent, "gita"),
            "with gita off the child's PATH, doctor must raise tool-missing for \
             gita; got:\n{absent}"
        );

        let present = doctor_json_on_tool_only_path(
            "gita",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            &["gita"],
        );
        assert!(
            !reports_tool_missing(&present, "gita"),
            "with a gita on the child's PATH, the finding must clear; got:\n{present}"
        );
    }

    /// A repo path containing a comma must be emitted as a properly-quoted CSV
    /// field and must round-trip through csv::Reader without corruption.
    /// Pre-fix, the concat-based writer produced a malformed row.
    #[test]
    fn csv_escaping_roundtrips_path_with_comma() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Use a repo path whose basename contains a comma. The manifest helper
        // only sets the `url` from the last path segment, so we synthesise the
        // manifest YAML directly to include an unusual path key.
        let yaml = "[repositories.\"github/owner/with,comma\"]\ntype = \"git\"\nurl = \"https://github.com/owner/withcomma.git\"\nversion = \"main\"\nrole = \"owned\"\n";
        let manifest = repoweave::manifest::Manifest::from_toml_str(yaml).unwrap();
        let project = repoweave::manifest::ProjectName::new("test-project").unwrap();
        let config = repoweave::manifest::IntegrationConfig::default();
        let cache = std::collections::HashMap::new();
        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            container_kind: ContainerKind::Primary,
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
        let tmp = common::tempdir().unwrap();
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
        let tmp = common::tempdir().unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // vscode-workspace uses all repos (not filtered by manifest file)
        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        integration.activate(&ctx).unwrap();
        assert!(root.join("test-project.code-workspace").exists());
    }

    #[test]
    fn generates_code_workspace_file_with_folders_and_settings() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Owned),
            ("github/chatly/web", Role::Owned),
        ]);
        let project = ProjectName::new("web-app").unwrap();
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
        // (was plain `true` previously — has_marker tolerates both forms).
        assert_eq!(parsed["rwv.generated"]["managed"], true);
    }

    #[test]
    fn project_name_appears_in_filename() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("my-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        integration.activate(&ctx).unwrap();
        assert!(root.join("my-project.code-workspace").exists());
    }

    #[test]
    fn preserves_user_customizations_on_reactivation() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Pre-existing rwv-managed workspace file with user customizations.
        // The marker is what makes this a re-activation rather than a seizure
        // of a hand-authored file.
        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "rwv.generated": true,
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
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
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
        let tmp = common::tempdir().unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No .code-workspace file present
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Active project has github/chatly/server.
        // github/acme/web is on disk but not in the project.
        let manifest = make_manifest(vec![("github/chatly/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();

        let all_repos_on_disk: Vec<RepoPath> = vec![
            RepoPath::new("github/chatly/server").expect("known-safe literal"),
            RepoPath::new("github/acme/web").expect("known-safe literal"),
        ];

        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            container_kind: ContainerKind::Primary,
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("proj-a").unwrap();
        let config = IntegrationConfig::default();

        let all_repos_on_disk: Vec<RepoPath> =
            vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
        let all_project_paths = vec!["proj-a".to_string(), "proj-b".to_string()];

        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            container_kind: ContainerKind::Primary,
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("hide-dotfiles = false");
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Active project has only github/acme/server.
        // All other repos are under github/other — should collapse to github/other.
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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
            container_kind: ContainerKind::Primary,
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
    // DefaultOnly semantics for git.* settings
    //
    // git.autoRepositoryDetection and git.repositoryScanMaxDepth are
    // DefaultOnly: rwv seeds them at greenfield but never overwrites a value
    // the user has explicitly set.
    // -----------------------------------------------------------------------

    /// git.* defaults are written on a fresh (empty) workspace.
    #[test]
    fn git_settings_seeded_on_fresh_workspace() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"], "subFolders",
            "default must be seeded on fresh workspace"
        );
        assert_eq!(
            parsed["settings"]["git.repositoryScanMaxDepth"], 3,
            "default depth must be seeded on fresh workspace"
        );
    }

    /// User-customized git.* values survive re-activation (DefaultOnly: no overwrite).
    #[test]
    fn git_settings_user_values_preserved_on_reactivate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Pre-existing workspace with user-customized git settings and rwv marker.
        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "rwv.generated": { "managed": true, "files.exclude": [] },
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "settings": {
    "git.autoRepositoryDetection": "always",
    "git.repositoryScanMaxDepth": 10,
    "files.exclude": { ".*": true }
  }
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"], "always",
            "user-set git.autoRepositoryDetection must not be overwritten on re-activate"
        );
        assert_eq!(
            parsed["settings"]["git.repositoryScanMaxDepth"], 10,
            "user-set git.repositoryScanMaxDepth must not be overwritten on re-activate"
        );
    }
}

// ===========================================================================
// vscode-workspace: residual-bug scenarios
// ===========================================================================
//
// Each scenario pins one promise of the activate/deactivate merge over a
// `.code-workspace`: what the user put there survives both, and rwv's own
// region — the `folders` entry whose path is ".", the marker, and the
// `files.exclude` keys the marker records — is the only thing either touches.

mod vscode_workspace_scenarios {
    use super::*;

    // -------------------------------------------------------------------------
    // Scenario 1 — User adds a personal `files.exclude` entry; sync must not
    // eat it.
    // -------------------------------------------------------------------------
    #[test]
    fn scenario1_user_files_exclude_survives_reactivation() {
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("foundations").unwrap();
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
            container_kind: ContainerKind::Primary,
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

        let content = std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap();
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
        assert_eq!(
            parsed["rwv.generated"]["managed"],
            serde_json::Value::Bool(true)
        );
        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"],
            "subFolders"
        );
    }

    // -------------------------------------------------------------------------
    // Scenario 2 — User adds `extensions`/`launch`/`tasks`/`compounds`; they
    // survive activate AND deactivate.
    // -------------------------------------------------------------------------
    #[test]
    fn scenario2_user_blocks_survive_activate_and_deactivate() {
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("myproject").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Activate: all four user blocks must survive.
        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("myproject.code-workspace")).unwrap();
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

        let content = std::fs::read_to_string(root.join("myproject.code-workspace")).unwrap();
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
    // -------------------------------------------------------------------------
    #[test]
    fn scenario3_user_extra_folder_survives_reactivation() {
        let tmp = common::tempdir().unwrap();
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
        let project = ProjectName::new("foundations").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let folders = parsed["folders"].as_array().unwrap();

        // BOTH folders must be present.
        assert_eq!(
            folders.len(),
            2,
            "both folders must be present after reactivation; got: {folders:?}"
        );

        // Element 0 must be the rwv-managed primary.
        assert_eq!(folders[0]["path"], ".", "primary folder must be at index 0");
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

        // Marker still present (object form).
        assert_eq!(
            parsed["rwv.generated"]["managed"],
            serde_json::Value::Bool(true)
        );
    }

    // -------------------------------------------------------------------------
    // Scenario 4 — Deactivate of a purely-rwv file deletes it; hand-written
    // file (no marker) is untouched.
    // -------------------------------------------------------------------------
    #[test]
    fn scenario4_deactivate_deletes_pure_rwv_file_leaves_handwritten() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // (a) A purely-rwv .code-workspace: marker + owned keys only. The
        // marker records the exclude keys, so all of them are rwv's own.
        write_file(
            root,
            "proj.code-workspace",
            r#"{
  "rwv.generated": {"managed": true, "files.exclude": [".*"]},
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
        let content = std::fs::read_to_string(root.join("mine.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["settings"]["editor.tabSize"],
            serde_json::Value::Number(2.into()),
            "hand-written file content must be byte-identical"
        );
    }

    // -------------------------------------------------------------------------
    // Scenario 5 — Deactivate strips rwv's own keys *within* the managed maps
    // and leaves everything the user put there.
    //
    // rwv owns keys within a managed map, never the whole map: the `folders`
    // entry whose path is ".", and the files.exclude keys the marker records.
    // -------------------------------------------------------------------------
    #[test]
    fn scenario5_deactivate_preserves_user_excludes_and_folders() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "foundations.code-workspace",
            r#"{
  "rwv.generated": {"managed": true, "files.exclude": [".*", "github/acme"]},
  "folders": [
    {"path": ".", "name": "foundations (primary)"},
    {"path": "../shared-notes", "name": "notes"}
  ],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {
      ".*": true,
      "github/acme": true,
      "**/target": true,
      "dist": false
    },
    "editor.tabSize": 2
  }
}"#,
        );

        VscodeWorkspace.deactivate(root).unwrap();

        let path = root.join("foundations.code-workspace");
        assert!(path.exists(), "file with user content must not be deleted");
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // The two rwv-derived exclude keys go; the two user keys stay, values
        // intact.
        let exclude = &parsed["settings"]["files.exclude"];
        assert!(
            exclude.get(".*").is_none() && exclude.get("github/acme").is_none(),
            "rwv-derived exclude keys must be stripped; got: {exclude}"
        );
        assert_eq!(
            exclude["**/target"],
            serde_json::Value::Bool(true),
            "user-added **/target must survive deactivate; got: {exclude}"
        );
        assert_eq!(
            exclude["dist"],
            serde_json::Value::Bool(false),
            "user-added dist must survive deactivate with its value; got: {exclude}"
        );

        // The primary folder entry goes; the user's extra root stays.
        let folders = parsed["folders"].as_array().unwrap();
        assert_eq!(
            folders.len(),
            1,
            "only the rwv primary entry may be stripped; got: {folders:?}"
        );
        assert_eq!(folders[0]["path"], "../shared-notes");
        assert_eq!(folders[0]["name"], "notes");

        // DefaultOnly git.* settings are never stripped, and unrelated
        // settings are untouched.
        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"],
            "subFolders"
        );
        assert_eq!(parsed["settings"]["git.repositoryScanMaxDepth"], 3);
        assert_eq!(parsed["settings"]["editor.tabSize"], 2);

        assert!(
            parsed.get("rwv.generated").is_none(),
            "marker must be stripped; got: {parsed}"
        );
    }

    // -------------------------------------------------------------------------
    // Scenario 6 — A git.* value the user changed is a user choice: it keeps
    // the file alive where the seeded value would not have.
    // -------------------------------------------------------------------------
    #[test]
    fn scenario6_deactivate_keeps_file_holding_user_changed_git_setting() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "proj.code-workspace",
            r#"{
  "rwv.generated": {"managed": true, "files.exclude": [".*"]},
  "folders": [{"path": ".", "name": "proj (primary)"}],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 10,
    "files.exclude": {".*": true}
  }
}"#,
        );

        VscodeWorkspace.deactivate(root).unwrap();

        let path = root.join("proj.code-workspace");
        assert!(
            path.exists(),
            "a git.* value the user changed must keep the file"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["settings"]["git.repositoryScanMaxDepth"], 10);
        assert!(parsed.get("rwv.generated").is_none());
        assert!(parsed.get("folders").is_none());
    }

    // -------------------------------------------------------------------------
    // Scenario 7 — A marker predating the recorded exclude list cannot say
    // which keys were rwv's, so it leaves all of them.
    // -------------------------------------------------------------------------
    #[test]
    fn scenario7_deactivate_leaves_excludes_when_marker_records_none() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "proj.code-workspace",
            r#"{
  "rwv.generated": true,
  "folders": [{"path": ".", "name": "proj (primary)"}],
  "settings": {
    "files.exclude": {".*": true, "dist": true}
  }
}"#,
        );

        VscodeWorkspace.deactivate(root).unwrap();

        let path = root.join("proj.code-workspace");
        assert!(path.exists(), "unattributable excludes must keep the file");
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            parsed["settings"]["files.exclude"],
            serde_json::json!({".*": true, "dist": true}),
            "no exclude key may be guessed at; got: {parsed}"
        );
        assert!(parsed.get("folders").is_none());
        assert!(parsed.get("rwv.generated").is_none());
    }

    // -------------------------------------------------------------------------
    // Scenario 8 — Activate is marker-gated too: a hand-authored workspace is
    // left byte-for-byte alone, not converted to an rwv-owned file.
    //
    // This is the take-the-pen escape hatch: delete rwv's marker and the file
    // is yours. Without this, `rwv doctor` reports the file USER-HELD and the
    // next intent verb silently seizes it.
    // -------------------------------------------------------------------------
    #[test]
    fn scenario8_activate_leaves_hand_authored_workspace_untouched() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // A workspace a user wrote by hand: no rwv.generated marker, a folder
        // layout and excludes that are entirely their own.
        write_file(
            root,
            "foundations.code-workspace",
            r#"{
  "folders": [
    {"path": "github/acme/server", "name": "server"},
    {"path": ".", "name": "my own name for the root"}
  ],
  "settings": {
    "git.repositoryScanMaxDepth": 7,
    "files.exclude": {"**/target": true},
    "editor.tabSize": 2
  }
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("foundations").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        contract::assert_activate_leaves_user_held_untouched(
            &root.join("foundations.code-workspace"),
            || VscodeWorkspace.activate(&ctx).unwrap(),
        );

        // Specifically: no marker was stamped, so the file does not become
        // rwv-owned on the run after next.
        let parsed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap(),
        )
        .unwrap();
        assert!(
            parsed.get("rwv.generated").is_none(),
            "activate must not stamp the marker on a user-held file; got: {parsed}"
        );
    }

    // -------------------------------------------------------------------------
    // Scenario 9 — The gate is the owned region, not the file. A file with no
    // `folders` has nothing rwv could be taking, so rwv creates the key and
    // manages from that point forward — preserving the blocks already there.
    // -------------------------------------------------------------------------
    #[test]
    fn scenario9_activate_adopts_unmarked_file_without_the_owned_region() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "foundations.code-workspace",
            r#"{
  "extensions": {"recommendations": ["rust-lang.rust-analyzer"]},
  "settings": {"editor.tabSize": 2}
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("foundations").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap(),
        )
        .unwrap();

        assert_eq!(
            parsed["rwv.generated"]["managed"], true,
            "an absent owned region is rwv's to create; got: {parsed}"
        );
        assert_eq!(parsed["folders"][0]["path"], ".");
        assert_eq!(parsed["folders"][0]["name"], "foundations (primary)");

        // The user's existing blocks are merged around, not replaced.
        assert_eq!(parsed["settings"]["editor.tabSize"], 2);
        assert_eq!(
            parsed["extensions"]["recommendations"][0],
            "rust-lang.rust-analyzer"
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

    #[cfg(unix)]
    #[test]
    fn npm_workspaces_activate_hook_runs_npm_install() {
        let (ok, ran) = activate_with_tool_shim(
            "npm-workspaces",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            "npm",
            0,
            &[],
        );
        assert!(ok, "activation must succeed when the hook's tool does");
        assert_eq!(
            ran.trim(),
            "install",
            "the hook must reach `npm install`; got: {ran:?}"
        );

        let (ok, ran) = activate_with_tool_shim(
            "npm-workspaces",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            "npm",
            1,
            &[],
        );
        assert!(!ok, "activation must fail when the hook's tool does");
        assert_eq!(
            ran.trim(),
            "install",
            "a failing tool is still a tool the hook reached; got: {ran:?}"
        );
    }

    #[test]
    fn npm_workspaces_activate_hook_noop_when_no_repos_detected() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No package.json in any repo
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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
    // cargo-workspace: the lockfile step
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn cargo_workspace_activate_hook_reaches_cargo_and_follows_its_exit() {
        let (ok, ran) = activate_with_tool_shim(
            "cargo-workspace",
            "github/acme/server/Cargo.toml",
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            "cargo",
            0,
            &["projects/app/Cargo.lock"],
        );
        assert!(ok, "activation must succeed when the hook's tool does");
        assert!(
            !ran.trim().is_empty(),
            "the hook must reach cargo; got: {ran:?}"
        );

        let (ok, ran) = activate_with_tool_shim(
            "cargo-workspace",
            "github/acme/server/Cargo.toml",
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            "cargo",
            1,
            &["projects/app/Cargo.lock"],
        );
        assert!(!ok, "activation must fail when the hook's tool does");
        assert!(
            !ran.trim().is_empty(),
            "a failing cargo is still a cargo the hook reached; got: {ran:?}"
        );
    }

    #[test]
    fn cargo_workspace_activate_hook_noop_when_no_repos_detected() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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

    /// When the cargo lockfile step fails, the error must hint at
    /// `integrations.cargo-workspace.exclude` and `members` config as the
    /// resolution paths for duplicate crate names.
    ///
    /// Drives the real `rwv` binary against a shimmed `cargo` that always
    /// exits 1, the way `hook_pin_survival_test.rs`'s
    /// `a_hooked_activation_runs_only_materializing_commands` puts a
    /// controlled PATH in front of a real subprocess: `Command::new("cargo")`
    /// resolves against whatever `PATH` the child process is started with,
    /// so a real binary earlier on that `PATH` is what makes the failure
    /// happen, rather than a string this test builds and checks against
    /// itself.
    ///
    /// Gated on the fixture, not the subject: the hint text is portable, but a
    /// `cargo` that reliably fails is a shebang shim on `PATH`, which Windows
    /// will neither find nor spawn.
    #[cfg(unix)]
    #[test]
    fn cargo_activate_hook_failure_names_exclude_and_members_hints() {
        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(ws.join("projects/app")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();

        let repo = ws.join("github/acme/server");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        git_init_with_commit(&repo);

        std::fs::write(
            ws.join("projects/app/rwv.toml"),
            "[repositories.\"github/acme/server\"]\ntype = \"git\"\n\
             url = \"https://github.com/acme/server.git\"\nversion = \"main\"\n\
             role = \"owned\"\n",
        )
        .unwrap();
        std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

        let rwv_with_path = |args: &[&str], path: &str| {
            common::rwv()
                .args(args)
                .current_dir(&ws)
                .env("PATH", path)
                .output()
                .expect("rwv should run")
        };
        let real_path = std::env::var("PATH").unwrap_or_default();

        // `activate` is a context verb: it surfaces and verifies but never
        // authors managed content (src/integrations/cargo_workspace.rs's own
        // activate_hook precheck says so). Only an authoring verb writes the
        // managed Cargo.toml the hook needs before it can even run — real
        // `cargo` (if any) on this process's PATH is fine here, since
        // nothing needs to resolve yet.
        let authored = rwv_with_path(&["doctor", "--fix"], &real_path);
        assert!(
            ws.join("projects/app/Cargo.toml").exists(),
            "fixture: the authoring pass should have written the managed Cargo.toml:\n{}\n{}",
            String::from_utf8_lossy(&authored.stdout),
            String::from_utf8_lossy(&authored.stderr)
        );

        // The run under audit: a `cargo` that always fails, ahead of
        // whatever else is on PATH.
        write_exit_code_shim(&bin, "cargo", 1);
        let shimmed_path = format!("{}:{}", bin.display(), real_path);

        let out = rwv_with_path(&["activate", "app"], &shimmed_path);
        let report = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        assert!(
            !out.status.success(),
            "activation must fail when the cargo hook does:\n{report}"
        );
        assert!(
            report.contains("integrations.cargo-workspace.exclude"),
            "error must name `integrations.cargo-workspace.exclude`:\n{report}"
        );
        assert!(
            report.contains("integrations.cargo-workspace.members"),
            "error must name `integrations.cargo-workspace.members`:\n{report}"
        );
        assert!(
            report.contains("include:"),
            "error must mention the `include:` list syntax:\n{report}"
        );
    }

    /// `cargo_workspace.rs`'s post-hook guard: cargo can report success and
    /// still leave the surfacing path holding a real file with the canonical
    /// lock still missing, if whatever it wrote replaced the dangling
    /// symlink `surface_symlinks` put there rather than writing through it.
    ///
    /// `activate_at` always runs `surface_symlinks` with
    /// `skip_missing_sources = false` immediately before any hook fires (see
    /// `activate_intent_at`'s doc comment) — on a first-ever activation the
    /// hook is always handed a freshly-created dangling symlink at this
    /// path. Real `cargo generate-lockfile` currently writes through it
    /// rather than replacing it (see
    /// `doctor_fix_in_a_workweave_generates_the_missing_cargo_lock`), but
    /// nothing in cargo's interface guarantees that, which is what the
    /// guard's own comment says. This shim reproduces the failure shape
    /// directly — deleting the symlink and writing a real file in its place
    /// before exiting 0 — to prove the check downstream of that state is
    /// still live, independent of whether today's cargo happens to trigger
    /// it on this host.
    ///
    /// Gated on the fixture, not the subject: the orphan check is portable,
    /// but a `cargo` that replaces the symlink on cue is a shebang shim on
    /// `PATH`, which Windows will neither find nor spawn.
    #[cfg(unix)]
    #[test]
    fn cargo_activate_hook_names_the_orphan_when_cargo_replaces_the_symlink() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(ws.join("projects/app")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();

        let repo = ws.join("github/acme/server");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        git_init_with_commit(&repo);

        std::fs::write(
            ws.join("projects/app/rwv.toml"),
            "[repositories.\"github/acme/server\"]\ntype = \"git\"\n\
             url = \"https://github.com/acme/server.git\"\nversion = \"main\"\n\
             role = \"owned\"\n",
        )
        .unwrap();
        std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

        // A `cargo` that reports success but replaces whatever
        // `surface_symlinks` just put at the surfacing path with a real
        // file — reproducing the exact state the post-hook guard checks
        // for, regardless of whether real cargo does this today.
        let fake_cargo = bin.join("cargo");
        std::fs::write(
            &fake_cargo,
            "#!/bin/sh\nrm -f Cargo.lock\nprintf '# fake lock\\n' > Cargo.lock\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_cargo, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = common::rwv()
            .args(["doctor", "--fix"])
            .current_dir(&ws)
            .env("PATH", &path)
            .output()
            .expect("rwv should run");
        let report = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        assert!(
            !ws.join("projects/app/Cargo.lock").exists(),
            "precondition for this arm: the canonical lock stays missing:\n{report}"
        );
        assert!(
            ws.join("Cargo.lock")
                .symlink_metadata()
                .map(|m| m.file_type().is_file())
                .unwrap_or(false),
            "precondition: the surfacing path should now be a real file, not the symlink \
             surface_symlinks created (symlink_metadata does not follow, so is_file() here \
             is already false for a symlink):\n{report}"
        );
        assert!(
            report.contains("wrote") && report.contains("but the canonical"),
            "the orphan guard should name what cargo wrote and that the canonical is still \
             missing:\n{report}"
        );
        assert!(
            report.contains("remove") && report.contains("re-run"),
            "the guard should name the repair:\n{report}"
        );
    }

    // -----------------------------------------------------------------------
    // uv-workspace: `uv sync`
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn uv_workspace_activate_hook_runs_uv_sync() {
        let (ok, ran) = activate_with_tool_shim(
            "uv-workspace",
            "github/acme/server/pyproject.toml",
            "[project]\nname = \"server\"\nversion = \"0.1.0\"\n",
            "uv",
            0,
            &[],
        );
        assert!(ok, "activation must succeed when the hook's tool does");
        assert_eq!(
            ran.trim(),
            "sync",
            "the hook must reach `uv sync`; got: {ran:?}"
        );

        let (ok, ran) = activate_with_tool_shim(
            "uv-workspace",
            "github/acme/server/pyproject.toml",
            "[project]\nname = \"server\"\nversion = \"0.1.0\"\n",
            "uv",
            1,
            &[],
        );
        assert!(!ok, "activation must fail when the hook's tool does");
        assert_eq!(
            ran.trim(),
            "sync",
            "a failing tool is still a tool the hook reached; got: {ran:?}"
        );
    }

    #[test]
    fn uv_workspace_activate_hook_noop_when_no_repos_detected() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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

    #[cfg(unix)]
    #[test]
    fn pnpm_workspaces_activate_hook_runs_pnpm_install() {
        let (ok, ran) = activate_with_tool_shim(
            "pnpm-workspaces",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            "pnpm",
            0,
            &[],
        );
        assert!(ok, "activation must succeed when the hook's tool does");
        assert_eq!(
            ran.trim(),
            "install",
            "the hook must reach `pnpm install`; got: {ran:?}"
        );

        let (ok, ran) = activate_with_tool_shim(
            "pnpm-workspaces",
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
            "pnpm",
            1,
            &[],
        );
        assert!(!ok, "activation must fail when the hook's tool does");
        assert_eq!(
            ran.trim(),
            "install",
            "a failing tool is still a tool the hook reached; got: {ran:?}"
        );
    }

    #[test]
    fn pnpm_workspaces_activate_hook_noop_when_no_repos_detected() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true");
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml(
            "enabled = true\nfiles = [\"turbo.json\", \".eslintrc.json\", \".prettierrc\"]",
        );
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let files = integration.generated_files(&ctx);
        assert_eq!(files, vec!["turbo.json", ".eslintrc.json", ".prettierrc"]);
    }

    #[test]
    fn generated_files_empty_when_no_files_configured() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let files = integration.generated_files(&ctx);
        assert!(files.is_empty());
    }

    #[test]
    fn activate_succeeds_when_files_exist() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Create the declared files in the project directory (output_dir)
        write_file(root, "turbo.json", r#"{"pipeline": {}}"#);
        write_file(root, ".eslintrc.json", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml(
            "enabled = true\nfiles = [\"turbo.json\", \".eslintrc.json\"]",
        );
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let result = integration.activate(&ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn activate_succeeds_even_when_files_missing() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Don't create the files — activate should still succeed (just warn)
        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\"turbo.json\"]");
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Create one of two declared files
        write_file(root, "turbo.json", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml(
            "enabled = true\nfiles = [\"turbo.json\", \".eslintrc.json\"]",
        );
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(root, "turbo.json", "{}");
        write_file(root, ".prettierrc", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml(
            "enabled = true\nfiles = [\"turbo.json\", \".prettierrc\"]",
        );
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn check_no_issues_when_no_files_configured() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn deactivate_succeeds() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let integration = StaticFiles;
        let result = integration.deactivate(root);
        assert!(result.is_ok());
    }

    #[test]
    fn activate_hook_is_noop() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\"turbo.json\"]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let result = integration.activate_hook(&ctx);
        assert!(
            result.is_ok(),
            "static-files activate hook should be a no-op"
        );
    }

    // ----- collision with workweave.link -----------

    /// Regression: when the same name is declared in both
    /// `static-files.files` and `workweave.link`, `activate()` MUST bail with a
    /// hard error rather than silently letting the framework's predicate
    /// tiebreak. The error message must name both integrations so the operator
    /// can act on it without re-reading the docs.
    #[test]
    fn activate_fails_when_name_collides_with_workweave_link() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // The static file exists — collision detection runs before existence
        // checks, so we'd rather not give activate() a way to fail for an
        // unrelated reason.
        write_file(root, ".beads", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\".beads\"]");
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

    /// Regression: `check()` MUST surface the collision as
    /// `Severity::Error` so `rwv doctor` fails loudly pre-activate (the
    /// signal that motivates the framework predicate).
    #[test]
    fn check_emits_error_for_workweave_link_collision() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\".beads\"]");
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");
        write_file(root, ".secrets", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml(
            "enabled = true\nfiles = [\".beads\", \".secrets\", \"turbo.json\"]",
        );
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\".beads\"]");
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
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");
        write_file(root, "turbo.json", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\"turbo.json\"]");
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
    /// `workweave:` section in rwv.toml).
    #[test]
    fn check_no_collision_when_workweave_absent() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, ".beads", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\".beads\"]");
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
    // static-files — RED scenarios
    // -----------------------------------------------------------------------
    //
    // is already covered above by
    // `activate_fails_when_name_collides_with_workweave_link` /
    // `check_emits_error_for_workweave_link_collision` (the C13 hard-error
    // path). This realizes the remaining plan scenarios:
    //
    // — deactivate strips only static-files-owned symlinks;
    // foreign symlinks and user files survive. The integration's
    // `deactivate(root)` is a no-op — symlink removal is the framework's job,
    // so the subject is `repoweave::activate::unsurface_names`.
    //
    // — missing declared file skipped with warning (already
    // covered by `check_warns_on_missing_files` and
    // `activate_succeeds_even_when_files_missing` above; we leave them in
    // place rather than duplicate).

    /// the framework's symlink reaping is owner-scoped on
    /// BOTH legs of its conjunction: a declared name is unlinked only when the
    /// name is one rwv surfaces AND the link's target is the shape activation
    /// would have written (`projects/<project>/<that name>`).
    ///
    /// The defect this catches is the target-shape leg going blind: a
    /// `workweave.link` entry is an absolute symlink into the source weave, so
    /// a name declared by both it and `static-files.files` — what an operator
    /// migrating a name between the two holds for one activation — would be
    /// unlinked out from under the operator by a name-only predicate.
    ///
    /// `tests/integration_framework_test.rs::owner_scoped_removal_preserves_unowned_symlinks`
    /// drives the other leg (an owner-shaped target at a name no integration
    /// claims) and is blind to this one.
    #[test]
    fn s6_static_files_2_deactivate_owner_scoped_symlink_removal() {
        use repoweave::symlink::{create as symlink_to, LinkTarget};

        let tmp = common::tempdir().unwrap();
        let root = tmp.path();
        let source_weave = common::tempdir().unwrap();

        // Surfaced out of the project the way activation writes it: a relative
        // `projects/<project>/<name>` target.
        write_file(root, "projects/test-project/.prettierrc", "{}\n");
        symlink_to(
            Path::new("projects/test-project/.prettierrc"),
            &root.join(".prettierrc"),
            LinkTarget::File,
        )
        .unwrap();

        // A workweave.link: an absolute link at a name the removal set also
        // names. Only the target shape distinguishes it.
        let foreign_target = source_weave.path().join("turbo.json");
        std::fs::write(&foreign_target, "{\"pipeline\": {}}\n").unwrap();
        symlink_to(&foreign_target, &root.join("turbo.json"), LinkTarget::File).unwrap();

        // A declared name the operator wrote by hand — not a link at all.
        let hand_written = "{\"extends\": \"../base\"}\n";
        write_file(root, ".eslintrc.json", hand_written);

        let names = vec![
            ".prettierrc".to_string(),
            "turbo.json".to_string(),
            ".eslintrc.json".to_string(),
        ];
        repoweave::activate::unsurface_names(root, &names).unwrap();

        assert!(
            root.join(".prettierrc").symlink_metadata().is_err(),
            "the surfaced static-files symlink must be removed"
        );

        assert_eq!(
            std::fs::read_link(root.join("turbo.json")).ok(),
            Some(foreign_target),
            "a declared name whose link points outside projects/<project>/ is \
             not rwv's surfacing and must survive"
        );

        assert_eq!(
            std::fs::read_to_string(root.join(".eslintrc.json")).ok(),
            Some(hand_written.to_string()),
            "a declared name the operator wrote as a real file must be \
             byte-identical"
        );
    }
}

// ===========================================================================
// Cross-port DefaultOnly regression battery
//
// For each port that adopts Ownership::DefaultOnly, two tests:
//   (a) s8_<port>_default_only_preserves_user_value — an existing value set by
//       the user (present in the file before activate) is not overwritten.
//   (b) s8_<port>_default_only_seeds_on_greenfield — a fresh / no-file case
//       gets a sensible non-literal default seeded.
//
// These tests use the same fixture-setup style (TempDir, write_file, make_ctx,
// Integration.activate()) and the same assertion shape across all ports, making
// the cross-port contract visible at a glance.  Each test also notes the
// port-specific equivalent added by the per-port spec, so reviewers can see
// that no coverage is duplicated — only the s8_ naming convention is new.
//
// Contract being tested (`Ownership::DefaultOnly` in src/integrations/merge.rs):
//   - merge_activate sets the key only when absent; never overwrites.
//   - strip_deactivate does NOT remove DefaultOnly keys.
//   - DefaultOnly drift is CLEAN in verify().
//   - DefaultOnly keys never appear in MergeResult::authored.
// ===========================================================================

mod s8_cross_port_default_only {
    use super::*;

    // -----------------------------------------------------------------------
    // npm — `name` and `private`
    //
    // Port-specific equivalents:
    //   (a) → regression_name_and_scripts_survive_activate
    //         default_only_private_false_survives_activate
    //   (b) → greenfield_name_set_from_context_project_name
    //
    // The s8 versions follow the uniform cross-port shape: a single DefaultOnly
    // key per test, minimal fixture, same assertion wording across ports.
    // -----------------------------------------------------------------------

    /// (a) npm — user-set `name` and `private: false` survive re-activate.
    ///
    /// The file already has the x-repoweave marker (indicating rwv previously
    /// authored the file).  DefaultOnly must NOT overwrite the existing values.
    #[test]
    fn s8_npm_default_only_preserves_user_value() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/api/package.json");

        // Pre-existing file: marker present, user-chosen name + private: false.
        write_file(
            root,
            "package.json",
            r#"{
  "x-repoweave": {"managed": true},
  "name": "acme-monorepo",
  "private": false,
  "workspaces": ["github/acme/api"]
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/api", Role::Owned)]);
        let project = ProjectName::new("different-project-name").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        NpmWorkspaces.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // DefaultOnly: existing values must survive — no overwrite.
        assert_eq!(
            parsed["name"], "acme-monorepo",
            "name (DefaultOnly) must not be overwritten on re-activate"
        );
        assert_eq!(
            parsed["private"], false,
            "private: false (DefaultOnly) must not be overwritten on re-activate"
        );
    }

    /// (b) npm — greenfield seeds `name` from project name and `private: true`.
    ///
    /// No pre-existing package.json.  DefaultOnly seeds sensible defaults.
    #[test]
    fn s8_npm_default_only_seeds_on_greenfield() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/api/package.json");

        // No root package.json — greenfield.
        assert!(!root.join("package.json").exists());

        let manifest = make_manifest(vec![("github/acme/api", Role::Owned)]);
        let project = ProjectName::new("my-workspace").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        NpmWorkspaces.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // DefaultOnly seeds from project name (not a hardcoded literal).
        assert_eq!(
            parsed["name"], "my-workspace",
            "greenfield name must be seeded from ctx.project (DefaultOnly)"
        );
        // DefaultOnly seeds private: true as the sensible default.
        assert_eq!(
            parsed["private"], true,
            "greenfield private must be seeded as true (DefaultOnly)"
        );
    }

    // -----------------------------------------------------------------------
    // uv — `[tool.uv].package`
    //
    // Port-specific equivalents:
    //   (a) → default_only_does_not_overwrite_user_set_package_true
    //   (b) → default_only_sets_package_false_on_greenfield
    // -----------------------------------------------------------------------

    /// (a) uv — user-set `[tool.uv].package = true` survives re-activate.
    ///
    /// DefaultOnly must not inject `package = false` when `package` already exists.
    #[test]
    fn s8_uv_default_only_preserves_user_value() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/astral/protocol/pyproject.toml");

        // Pre-existing file: marker on members, user-set package = true.
        write_file(
            root,
            "pyproject.toml",
            concat!(
                "[tool.uv.workspace]\n",
                "# managed by rwv\n",
                "members = [\"github/astral/protocol\"]\n",
                "\n",
                "[tool.uv]\n",
                "package = true\n",
            ),
        );

        let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        UvWorkspace.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();

        // DefaultOnly: user-set value must survive.
        assert!(
            after.contains("package = true"),
            "user-set package=true must survive activate (DefaultOnly never overwrites); got:\n{after}"
        );
        assert!(
            !after.contains("package = false"),
            "DefaultOnly must not inject package=false when key is present; got:\n{after}"
        );
    }

    /// (b) uv — greenfield seeds `[tool.uv].package = false`.
    ///
    /// No pre-existing pyproject.toml.  DefaultOnly seeds `package = false`
    /// so `uv sync` accepts a non-package root.
    #[test]
    fn s8_uv_default_only_seeds_on_greenfield() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/astral/protocol/pyproject.toml");

        // No root pyproject.toml — greenfield.
        assert!(!root.join("pyproject.toml").exists());

        let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        UvWorkspace.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();

        // DefaultOnly seeds package = false on a fresh file.
        assert!(
            after.contains("package = false") || after.contains("package=false"),
            "greenfield pyproject.toml must get package=false from DefaultOnly; got:\n{after}"
        );
    }

    // -----------------------------------------------------------------------
    // cargo — `[workspace].resolver`
    //
    // Port-specific equivalents:
    //   (a) → resolver_default_only_operator_override_preserved
    //         (in s7_cargo_doctor mod)
    //   (b) → resolver_default_only_greenfield_sets_resolver_2
    //         (in s7_cargo_doctor mod)
    // -----------------------------------------------------------------------

    /// (a) cargo — user-set `resolver = "1"` survives re-activate.
    ///
    /// DefaultOnly must not overwrite an existing resolver value,
    /// even when the rwv marker is present on members.
    #[test]
    fn s8_cargo_default_only_preserves_user_value() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");

        // Pre-existing Cargo.toml: marker on members, user-set resolver = "1".
        write_file(
            root,
            "Cargo.toml",
            "[workspace]\n# managed by rwv\nmembers = [\"github/acme/server\"]\n\
             # managed by rwv\nresolver = \"1\"\n",
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();

        // DefaultOnly: operator's resolver = "1" must survive.
        assert!(
            content.contains("resolver = \"1\""),
            "resolver = \"1\" (user-set DefaultOnly) must survive activate; got:\n{content}"
        );
        assert!(
            !content.contains("resolver = \"2\""),
            "DefaultOnly must not overwrite resolver to \"2\"; got:\n{content}"
        );
    }

    /// (b) cargo — greenfield seeds `resolver = "2"`.
    ///
    /// No pre-existing Cargo.toml.  DefaultOnly seeds resolver = "2".
    #[test]
    fn s8_cargo_default_only_seeds_on_greenfield() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");

        // No root Cargo.toml — greenfield.
        assert!(!root.join("Cargo.toml").exists());

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        CargoWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();

        // DefaultOnly seeds resolver = "2" on a fresh file.
        assert!(
            content.contains("resolver = \"2\""),
            "greenfield Cargo.toml must get resolver = \"2\" from DefaultOnly; got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // go.work — `go <version>`
    //
    // Port-specific equivalents: live in src/integrations/go_work.rs (unit
    // tests internal to the port module).  These s8 tests exercise the same
    // contract from the integration-test layer using GoWork.activate() directly,
    // mirroring the cross-port shape.
    //
    //   (a) → go_work.rs::regression_no_downgrade_defaultonly_preserves_existing_go_line
    //   (b) → go_work.rs::greenfield_go_line_written_from_max_go_version
    //
    // Note: because FORCE_GOWORK_FALLBACK is a thread-local private to the
    // go_work module, these tests go through the public activate() entrypoint.
    // If `go` is on PATH the primary path is used; otherwise the hand-parse
    // fallback.  Both paths honour the DefaultOnly contract.
    // -----------------------------------------------------------------------

    /// (a) go.work — existing `go 1.20` line survives re-activate.
    ///
    /// The member go.mod also declares `go 1.20`, so `max_go_version` computes
    /// 1.20 whether `go` is on PATH (primary path: `go work edit -go=1.20`) or
    /// not (fallback path: DefaultOnly preserves the existing 1.20).  In both
    /// cases the go-line in the output must still be `go 1.20`.
    ///
    /// 1.20 (not this file's usual 1.26): this test goes through activate()
    /// with `go` on PATH, and 1.21 is the oldest go release with GOTOOLCHAIN
    /// switching, so a fixture at or below that never makes `go work` reach
    /// the network for a toolchain download.
    ///
    /// Note: the deeper DefaultOnly contract (preserving user-set version even
    /// when it differs from max_go_version) is fully tested in the fallback-
    /// path unit tests inside go_work.rs
    /// (`regression_no_downgrade_defaultonly_preserves_existing_go_line`), which
    /// force the fallback via a thread-local.  The s8 test here validates the
    /// cross-port shape through the public Integration::activate() entrypoint.
    #[test]
    fn s8_go_work_default_only_preserves_user_value() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Member go.mod declares go 1.20 — same version as the go.work.
        // max_go_version will compute 1.20, so both primary and fallback paths
        // produce "go 1.20" and neither downgrades it.
        write_file(
            root,
            "github/acme/server/go.mod",
            "module github.com/acme/server\n\ngo 1.20\n",
        );

        // Pre-existing go.work with go 1.20.
        write_file(
            root,
            "go.work",
            "go 1.20\n\n// managed by repoweave\nuse (\n\t./github/acme/server\n)\n",
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        GoWork.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();

        // go 1.20 must be present after activate (not downgraded, not removed).
        assert!(
            content.contains("go 1.20"),
            "go 1.20 must survive activate; got:\n{content}"
        );
        // Confirm the marker is still present (Author key managed correctly).
        assert!(
            content.contains("// managed by repoweave"),
            "ownership marker must be present after activate; got:\n{content}"
        );
    }

    /// (b) go.work — greenfield seeds `go <version>` from member go.mod files.
    ///
    /// No pre-existing go.work.  DefaultOnly seeds the go-line from
    /// `max_go_version` across member go.mod files (a sensible non-literal default).
    ///
    /// 1.20 (not this file's usual 1.26): this test goes through activate()
    /// with `go` on PATH, and 1.21 is the oldest go release with GOTOOLCHAIN
    /// switching, so a fixture at or below that never makes `go work` reach
    /// the network for a toolchain download.
    #[test]
    fn s8_go_work_default_only_seeds_on_greenfield() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Member go.mod declares go 1.20.
        write_file(
            root,
            "github/acme/server/go.mod",
            "module github.com/acme/server\n\ngo 1.20\n",
        );

        // No go.work — greenfield.
        assert!(!root.join("go.work").exists());

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        GoWork.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();

        // DefaultOnly seeds go version from max_go_version (not a hardcoded
        // literal): restore_go_directive puts the value rwv computed back after
        // `go work use`, so the seed is exactly what the go.mod reported, on
        // both the tool and hand-edit paths.
        assert!(
            content.contains("go 1.20"),
            "greenfield go.work must seed go 1.20 from max_go_version; got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // vscode — `git.autoRepositoryDetection`
    //
    // Port-specific equivalents:
    //   (a) → git_settings_user_values_preserved_on_reactivate
    //   (b) → git_settings_seeded_on_fresh_workspace
    //         (both in vscode_workspace mod of this file)
    // -----------------------------------------------------------------------

    /// (a) vscode — user-customized `git.autoRepositoryDetection` survives re-activate.
    ///
    /// The user has set `git.autoRepositoryDetection` to "always" (not the rwv
    /// default "subFolders").  DefaultOnly must not overwrite it.
    #[test]
    fn s8_vscode_default_only_preserves_user_value() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Pre-existing workspace: marker present, user-set git.* values.
        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "rwv.generated": { "managed": true, "files.exclude": [] },
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "settings": {
    "git.autoRepositoryDetection": "always",
    "git.repositoryScanMaxDepth": 10,
    "files.exclude": { ".*": true }
  }
}"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // DefaultOnly: user-set values must survive.
        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"], "always",
            "user-set git.autoRepositoryDetection (DefaultOnly) must not be overwritten"
        );
        assert_eq!(
            parsed["settings"]["git.repositoryScanMaxDepth"], 10,
            "user-set git.repositoryScanMaxDepth (DefaultOnly) must not be overwritten"
        );
    }

    /// (b) vscode — greenfield seeds `git.autoRepositoryDetection = "subFolders"`.
    ///
    /// No pre-existing .code-workspace.  DefaultOnly seeds the git.* settings
    /// to their sensible defaults.
    #[test]
    fn s8_vscode_default_only_seeds_on_greenfield() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No .code-workspace — greenfield.
        assert!(!root.join("test-project.code-workspace").exists());

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // DefaultOnly seeds the expected defaults on a fresh workspace.
        assert_eq!(
            parsed["settings"]["git.autoRepositoryDetection"], "subFolders",
            "greenfield workspace must get git.autoRepositoryDetection = \"subFolders\" from DefaultOnly"
        );
        assert_eq!(
            parsed["settings"]["git.repositoryScanMaxDepth"], 3,
            "greenfield workspace must get git.repositoryScanMaxDepth = 3 from DefaultOnly"
        );
    }
}

// ===========================================================================
// doctor verify() — npm-workspaces
// ===========================================================================

mod s7_npm_doctor {
    use super::*;
    use repoweave::integrations::NpmWorkspaces;

    // -----------------------------------------------------------------------
    // MISSING: verify() reports MISSING when package.json is absent
    // -----------------------------------------------------------------------

    /// Given: npm repos detected but package.json absent.
    /// Then:  verify() reports a single MISSING+safe_to_fix finding.
    #[test]
    fn s7_npm_doctor_missing_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one MISSING issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
        assert!(
            issue.message.contains("missing"),
            "MISSING issue message should contain 'missing': {}",
            issue.message
        );
        assert!(
            issue.message.contains("rwv doctor --fix"),
            "MISSING issue message should mention 'rwv doctor --fix': {}",
            issue.message
        );
    }

    /// Given: MISSING package.json.
    /// When:  activate() runs (simulating doctor --fix).
    /// Then:  package.json created with x-repoweave marker; verify() returns CLEAN.
    #[test]
    fn s7_npm_doctor_missing_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: MISSING.
        let pre_issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(pre_issues.len(), 1, "expected MISSING pre-condition");
        assert!(pre_issues[0].safe_to_fix);

        // Simulate doctor --fix.
        NpmWorkspaces.activate(&ctx).unwrap();

        let pkg_path = root.join("package.json");
        assert!(
            pkg_path.exists(),
            "package.json must be created after activate"
        );

        let content = std::fs::read_to_string(&pkg_path).unwrap();
        assert!(
            content.contains("x-repoweave"),
            "package.json must have x-repoweave marker after activate: {content}"
        );

        // Post-condition: CLEAN.
        let post_issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert!(
            post_issues.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post_issues:?}"
        );
    }

    // -----------------------------------------------------------------------
    // DRIFT: verify() reports DRIFT when marker present but content differs
    // -----------------------------------------------------------------------

    /// Given: package.json with x-repoweave marker but outdated workspaces list.
    /// Then:  verify() reports a single DRIFT+safe_to_fix finding.
    #[test]
    fn s7_npm_doctor_drift_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Write package.json with marker but only one workspace (outdated).
        write_file(
            root,
            "package.json",
            r#"{"x-repoweave":{"managed":true},"name":"test-project","private":true,"workspaces":["github/acme/server"]}"#,
        );

        // Both repos have package.json on disk.
        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/web/package.json");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one DRIFT issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
        assert!(
            issue.message.contains("drift"),
            "DRIFT issue message should contain 'drift': {}",
            issue.message
        );
    }

    /// Given: DRIFT package.json.
    /// When:  activate() runs.
    /// Then:  verify() returns CLEAN.
    #[test]
    fn s7_npm_doctor_drift_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "package.json",
            r#"{"x-repoweave":{"managed":true},"name":"test-project","private":true,"workspaces":["github/acme/server"]}"#,
        );
        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/web/package.json");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: DRIFT.
        let pre_issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(pre_issues.len(), 1, "expected DRIFT pre-condition");

        // Simulate fix.
        NpmWorkspaces.activate(&ctx).unwrap();

        // Post-condition: CLEAN.
        let post_issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert!(
            post_issues.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post_issues:?}"
        );

        let content = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let ws = parsed["workspaces"].as_array().unwrap();
        assert_eq!(ws.len(), 2, "both repos must be in workspaces after fix");
    }

    // -----------------------------------------------------------------------
    // USER-HELD: verify() reports USER-HELD, doctor --fix is a no-op
    // -----------------------------------------------------------------------

    /// Given: package.json with workspaces but NO x-repoweave marker.
    /// Then:  verify() reports USER-HELD+!safe_to_fix.
    #[test]
    fn s7_npm_doctor_user_held_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No x-repoweave marker — user holds the pen.
        write_file(
            root,
            "package.json",
            r#"{"name":"test-project","private":true,"workspaces":["github/acme/server"]}"#,
        );
        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            !issue.safe_to_fix,
            "USER-HELD issue must NOT be safe_to_fix"
        );
        assert!(
            issue.message.contains("NOT auto-take-over")
                || issue.message.contains("not auto")
                || issue.message.contains("unmarked"),
            "USER-HELD message must describe no-takeover: {}",
            issue.message
        );
    }

    /// Given: USER-HELD package.json.
    /// When:  activate() runs (merge's own guard).
    /// Then:  The workspaces content is left intact (merge defers to user).
    #[test]
    fn s7_npm_doctor_user_held_file_unchanged_after_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let original =
            r#"{"name":"test-project","private":true,"workspaces":["github/acme/server"]}"#;
        write_file(root, "package.json", original);
        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Verify reports USER-HELD with safe_to_fix=false.
        let issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(
            !issues[0].safe_to_fix,
            "must be USER-HELD (not safe_to_fix)"
        );

        // Even if activate() is called, the workspaces key is left intact.
        NpmWorkspaces.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("package.json")).unwrap();
        // Merge defers: the user's workspaces array is not overwritten.
        assert!(
            !after.contains("x-repoweave"),
            "user-held file must NOT have x-repoweave marker added by activate: {after}"
        );
    }

    // -----------------------------------------------------------------------
    // CLEAN: verify() returns no issues when file is up to date
    // -----------------------------------------------------------------------

    /// Given: package.json was written by activate() (marker + correct content).
    /// Then:  verify() returns no issues (CLEAN).
    #[test]
    fn s7_npm_doctor_clean_after_fresh_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        NpmWorkspaces.activate(&ctx).unwrap();

        let issues = NpmWorkspaces.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "verify() must return no issues for a freshly-activated package.json, got: {issues:?}"
        );
    }
}

// ===========================================================================
// doctor verify() — pnpm-workspaces
// ===========================================================================

mod s7_pnpm_doctor {
    use super::*;
    use repoweave::integrations::PnpmWorkspaces;

    // -----------------------------------------------------------------------
    // MISSING
    // -----------------------------------------------------------------------

    /// Given: pnpm repos detected but pnpm-workspace.yaml absent.
    /// Then:  verify() reports a single MISSING+safe_to_fix finding.
    #[test]
    fn s7_pnpm_doctor_missing_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = PnpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one MISSING issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
        assert!(
            issue.message.contains("missing"),
            "MISSING issue message should contain 'missing': {}",
            issue.message
        );
    }

    /// Given: MISSING pnpm-workspace.yaml.
    /// When:  activate() runs.
    /// Then:  file created with marker; verify() returns CLEAN.
    #[test]
    fn s7_pnpm_doctor_missing_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: MISSING.
        let pre = PnpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "expected MISSING pre-condition");
        assert!(pre[0].safe_to_fix);

        PnpmWorkspaces.activate(&ctx).unwrap();

        let path = root.join("pnpm-workspace.yaml");
        assert!(
            path.exists(),
            "pnpm-workspace.yaml must exist after activate"
        );

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("# managed by repoweave"),
            "file must have marker after activate: {content}"
        );

        let post = PnpmWorkspaces.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post:?}"
        );
    }

    // -----------------------------------------------------------------------
    // DRIFT
    // -----------------------------------------------------------------------

    /// Given: pnpm-workspace.yaml with marker but outdated packages list.
    /// Then:  verify() reports DRIFT+safe_to_fix.
    #[test]
    fn s7_pnpm_doctor_drift_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pnpm-workspace.yaml",
            "# managed by repoweave\npackages:\n  - github/acme/server\n",
        );
        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/web/package.json");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = PnpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one DRIFT issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
        assert!(
            issue.message.contains("drift"),
            "DRIFT message should contain 'drift': {}",
            issue.message
        );
    }

    /// Given: DRIFT pnpm-workspace.yaml.
    /// When:  activate() runs.
    /// Then:  verify() returns CLEAN.
    #[test]
    fn s7_pnpm_doctor_drift_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pnpm-workspace.yaml",
            "# managed by repoweave\npackages:\n  - github/acme/server\n",
        );
        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/web/package.json");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: DRIFT.
        let pre = PnpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "expected DRIFT pre-condition");

        PnpmWorkspaces.activate(&ctx).unwrap();

        let post = PnpmWorkspaces.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post:?}"
        );
    }

    // -----------------------------------------------------------------------
    // USER-HELD
    // -----------------------------------------------------------------------

    /// Given: pnpm-workspace.yaml with packages: but NO marker.
    /// Then:  verify() reports USER-HELD+!safe_to_fix.
    #[test]
    fn s7_pnpm_doctor_user_held_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No marker line.
        write_file(
            root,
            "pnpm-workspace.yaml",
            "packages:\n  - github/acme/server\n",
        );
        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = PnpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            !issue.safe_to_fix,
            "USER-HELD issue must NOT be safe_to_fix"
        );
        assert!(
            issue.message.contains("NOT auto-take-over")
                || issue.message.contains("not auto")
                || issue.message.contains("unmarked"),
            "USER-HELD message must describe no-takeover: {}",
            issue.message
        );
    }

    /// Given: USER-HELD pnpm-workspace.yaml.
    /// When:  activate() runs (merge's guard).
    /// Then:  The file is byte-identical and still USER-HELD — activate never
    ///        takes the pen from a file it does not already hold.
    #[test]
    fn s7_pnpm_doctor_user_held_file_unchanged_after_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let original = "packages:\n  - github/acme/server\n";
        write_file(root, "pnpm-workspace.yaml", original);
        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = PnpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(!issues[0].safe_to_fix, "must be USER-HELD");

        PnpmWorkspaces.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        assert_eq!(
            after, original,
            "activate must leave a USER-HELD pnpm-workspace.yaml byte-identical"
        );

        let post = PnpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(post.len(), 1, "expected the USER-HELD finding to persist");
        assert!(
            !post[0].safe_to_fix,
            "post-activate finding must still be USER-HELD, got: {post:?}"
        );
    }

    // -----------------------------------------------------------------------
    // CLEAN
    // -----------------------------------------------------------------------

    /// Given: pnpm-workspace.yaml was written by activate() (marker + correct content).
    /// Then:  verify() returns no issues (CLEAN).
    #[test]
    fn s7_pnpm_doctor_clean_after_fresh_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        PnpmWorkspaces.activate(&ctx).unwrap();

        let issues = PnpmWorkspaces.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "verify() must return no issues for a freshly-activated pnpm-workspace.yaml, got: {issues:?}"
        );
    }

    // -----------------------------------------------------------------------
    // CLEAN — duplicate/overlapping globs must not cause false DRIFT
    // -----------------------------------------------------------------------

    /// Regression: when a member repo's pnpm-workspace.yaml has duplicate
    /// glob entries, expand_workspace_entries() produces a list with repeated
    /// items.  activate() writes the deduped set (via OwnedValue::sorted_array)
    /// but the old verify() only sorted — not deduped — its expected list,
    /// making a CLEAN file look like DRIFT.
    ///
    /// Given: member repo whose pnpm-workspace.yaml lists the same glob twice.
    /// When:  activate() runs (on-disk is deduped), then verify() runs.
    /// Then:  verify() returns no issues (CLEAN).
    #[test]
    fn s7_pnpm_doctor_clean_when_member_has_duplicate_globs() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Multi-package repo whose pnpm-workspace.yaml repeats "packages/*".
        // expand_workspace_entries() will emit the glob twice; activate() dedupes
        // it before writing.  verify() must also dedup before comparing.
        touch(root, "github/acme/mono/package.json");
        write_file(
            root,
            "github/acme/mono/pnpm-workspace.yaml",
            "packages:\n  - packages/*\n  - packages/*\n",
        );

        let manifest = make_manifest(vec![("github/acme/mono", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // activate() writes the deduped, sorted set.
        PnpmWorkspaces.activate(&ctx).unwrap();

        // verify() must agree with what activate() wrote — no false DRIFT.
        let issues = PnpmWorkspaces.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "verify() must return CLEAN when on-disk matches the deduped member globs, got: {issues:?}"
        );
    }
}

// ===========================================================================
// doctor verify() — uv-workspace
// ===========================================================================

mod s7_uv_doctor {
    use super::*;
    use repoweave::integrations::UvWorkspace;

    // -----------------------------------------------------------------------
    // MISSING
    // -----------------------------------------------------------------------

    /// Given: Python repos detected but pyproject.toml absent.
    /// Then:  verify() reports a single MISSING+safe_to_fix finding.
    #[test]
    fn s7_uv_doctor_missing_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = UvWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one MISSING issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
        assert!(
            issue.message.contains("missing"),
            "MISSING message should contain 'missing': {}",
            issue.message
        );
        assert!(
            issue.message.contains("rwv doctor --fix"),
            "MISSING message should mention 'rwv doctor --fix': {}",
            issue.message
        );
    }

    /// Given: MISSING pyproject.toml.
    /// When:  activate() runs.
    /// Then:  file created with marker; verify() returns CLEAN.
    #[test]
    fn s7_uv_doctor_missing_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: MISSING.
        let pre = UvWorkspace.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "expected MISSING pre-condition");
        assert!(pre[0].safe_to_fix);

        UvWorkspace.activate(&ctx).unwrap();

        let path = root.join("pyproject.toml");
        assert!(
            path.exists(),
            "pyproject.toml must be created after activate"
        );

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("# managed by rwv"),
            "file must have '# managed by rwv' marker after activate: {content}"
        );
        assert!(
            content.contains("github/acme/server"),
            "members must include the repo: {content}"
        );

        let post = UvWorkspace.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post:?}"
        );
    }

    // -----------------------------------------------------------------------
    // DRIFT
    // -----------------------------------------------------------------------

    /// Given: pyproject.toml with marker but outdated members list.
    /// Then:  verify() reports DRIFT+safe_to_fix.
    #[test]
    fn s7_uv_doctor_drift_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Seed: only server in members (drift — web was added to manifest).
        write_file(
            root,
            "pyproject.toml",
            "[tool.uv.workspace]\n# managed by rwv\nmembers = [\"github/acme/server\"]\n",
        );
        touch(root, "github/acme/server/pyproject.toml");
        touch(root, "github/acme/web/pyproject.toml");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = UvWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one DRIFT issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
        assert!(
            issue.message.contains("drift"),
            "DRIFT message should contain 'drift': {}",
            issue.message
        );
    }

    /// Given: DRIFT pyproject.toml.
    /// When:  activate() runs.
    /// Then:  verify() returns CLEAN.
    #[test]
    fn s7_uv_doctor_drift_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pyproject.toml",
            "[tool.uv.workspace]\n# managed by rwv\nmembers = [\"github/acme/server\"]\n",
        );
        touch(root, "github/acme/server/pyproject.toml");
        touch(root, "github/acme/web/pyproject.toml");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: DRIFT.
        let pre = UvWorkspace.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "expected DRIFT pre-condition");

        UvWorkspace.activate(&ctx).unwrap();

        let post = UvWorkspace.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post:?}"
        );

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(
            content.contains("github/acme/web"),
            "web must be in members after fix: {content}"
        );
    }

    // -----------------------------------------------------------------------
    // USER-HELD
    // -----------------------------------------------------------------------

    /// Given: pyproject.toml with [tool.uv.workspace].members but NO marker.
    /// Then:  verify() reports USER-HELD+!safe_to_fix.
    #[test]
    fn s7_uv_doctor_user_held_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No "# managed by rwv" marker.
        write_file(
            root,
            "pyproject.toml",
            "[tool.uv.workspace]\nmembers = [\"github/acme/server\"]\n",
        );
        touch(root, "github/acme/server/pyproject.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = UvWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            !issue.safe_to_fix,
            "USER-HELD issue must NOT be safe_to_fix"
        );
        assert!(
            issue.message.contains("NOT auto-take-over")
                || issue.message.contains("not auto")
                || issue.message.contains("unmarked"),
            "USER-HELD message must describe no-takeover: {}",
            issue.message
        );
    }

    /// Given: USER-HELD pyproject.toml.
    /// When:  activate() runs (merge's guard).
    /// Then:  The members key is NOT clobbered.
    #[test]
    fn s7_uv_doctor_user_held_file_unchanged_after_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let original = "[tool.uv.workspace]\nmembers = [\"github/acme/server\"]\n";
        write_file(root, "pyproject.toml", original);
        touch(root, "github/acme/server/pyproject.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = UvWorkspace.verify(&ctx).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(!issues[0].safe_to_fix, "must be USER-HELD");

        // Even if activate() is called, the members key must not be overwritten.
        UvWorkspace.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(
            !after.contains("# managed by rwv"),
            "user-held file must NOT get rwv marker from activate: {after}"
        );
    }

    // -----------------------------------------------------------------------
    // CLEAN
    // -----------------------------------------------------------------------

    /// Given: pyproject.toml written by activate() (marker + correct members).
    /// Then:  verify() returns no issues (CLEAN).
    #[test]
    fn s7_uv_doctor_clean_after_fresh_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        UvWorkspace.activate(&ctx).unwrap();

        let issues = UvWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "verify() must return no issues for a freshly-activated pyproject.toml, got: {issues:?}"
        );
    }
}

// ===========================================================================
// doctor verify() — go-work
// ===========================================================================

mod s7_go_work_doctor {
    use super::*;
    use repoweave::integrations::GoWork;

    // Whether these fixtures exercise go-work's `go work` path or its
    // hand-parse fallback is decided by whether `go` is on PATH here, and this
    // file cannot decide it: the `FORCE_GOWORK_FALLBACK` override is
    // `#[cfg(test)]`-gated inside the library, so neither it nor the branch
    // reading it exists in the build an integration test links against.
    // Measured: forcing either answer leaves all seven green, so no assertion
    // below separates the two paths — treat a green here as saying nothing
    // about which one ran.

    fn write_go_mod(root: &Path, repo: &str) {
        write_file(
            root,
            &format!("{repo}/go.mod"),
            "module example.com/x\n\ngo 1.20\n",
        );
    }

    // -----------------------------------------------------------------------
    // MISSING
    // -----------------------------------------------------------------------

    /// Given: Go repos detected but go.work absent.
    /// Then:  verify() reports a single MISSING+safe_to_fix finding.
    #[test]
    fn s7_go_work_doctor_missing_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_go_mod(root, "github/acme/server");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = GoWork.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one MISSING issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
        assert!(
            issue.message.contains("missing"),
            "MISSING message should contain 'missing': {}",
            issue.message
        );
    }

    /// Given: MISSING go.work.
    /// When:  activate() runs.
    /// Then:  file created with marker; verify() returns CLEAN.
    #[test]
    fn s7_go_work_doctor_missing_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_go_mod(root, "github/acme/server");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: MISSING.
        let pre = GoWork.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "expected MISSING pre-condition");
        assert!(pre[0].safe_to_fix);

        GoWork.activate(&ctx).unwrap();

        let path = root.join("go.work");
        assert!(path.exists(), "go.work must be created after activate");

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("// managed by repoweave"),
            "go.work must have '// managed by repoweave' marker: {content}"
        );

        let post = GoWork.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post:?}"
        );
    }

    // -----------------------------------------------------------------------
    // DRIFT
    // -----------------------------------------------------------------------

    /// Given: go.work with marker but outdated use entries.
    /// Then:  verify() reports DRIFT+safe_to_fix.
    #[test]
    fn s7_go_work_doctor_drift_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "go.work",
            concat!(
                "go 1.20\n\n",
                "// managed by repoweave\n",
                "use (\n",
                "\t./github/acme/server\n",
                ")\n"
            ),
        );
        write_go_mod(root, "github/acme/server");
        write_go_mod(root, "github/acme/web");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = GoWork.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one DRIFT issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
        assert!(
            issue.message.contains("drift"),
            "DRIFT message should contain 'drift': {}",
            issue.message
        );
    }

    /// Given: DRIFT go.work.
    /// When:  activate() runs.
    /// Then:  verify() returns CLEAN.
    #[test]
    fn s7_go_work_doctor_drift_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "go.work",
            concat!(
                "go 1.20\n\n",
                "// managed by repoweave\n",
                "use (\n",
                "\t./github/acme/server\n",
                ")\n"
            ),
        );
        write_go_mod(root, "github/acme/server");
        write_go_mod(root, "github/acme/web");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/web", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: DRIFT.
        let pre = GoWork.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "expected DRIFT pre-condition");

        GoWork.activate(&ctx).unwrap();

        let post = GoWork.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post:?}"
        );

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            content.contains("github/acme/web"),
            "web must be in use entries after fix: {content}"
        );
    }

    // -----------------------------------------------------------------------
    // USER-HELD
    // -----------------------------------------------------------------------

    /// Given: go.work with use block but NO `// managed by repoweave` marker.
    /// Then:  verify() reports USER-HELD+!safe_to_fix.
    #[test]
    fn s7_go_work_doctor_user_held_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No marker.
        write_file(
            root,
            "go.work",
            "go 1.20\n\nuse (\n\t./github/acme/server\n)\n",
        );
        write_go_mod(root, "github/acme/server");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = GoWork.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            !issue.safe_to_fix,
            "USER-HELD issue must NOT be safe_to_fix"
        );
        assert!(
            issue.message.contains("NOT auto-take-over")
                || issue.message.contains("not auto")
                || issue.message.contains("unmarked"),
            "USER-HELD message must describe no-takeover: {}",
            issue.message
        );
    }

    /// Given: USER-HELD go.work (use block present, no marker).
    /// When:  activate() runs (forced-fallback path).
    /// Then:  the file is byte-for-byte unchanged — the ownership guard
    ///        short-circuits before any mutation.
    ///
    /// This is the parity test with cargo-workspace's equivalent invariant:
    /// a present-but-unmarked managed file is left strictly alone.
    #[test]
    fn s7_go_work_doctor_user_held_file_unchanged_after_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let original = "go 1.20\n\nuse (\n\t./github/acme/server\n)\n";
        write_file(root, "go.work", original);
        write_go_mod(root, "github/acme/server");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Verify the pre-condition: USER-HELD detected before activate.
        let issues = GoWork.verify(&ctx).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(
            !issues[0].safe_to_fix,
            "must be USER-HELD (safe_to_fix=false)"
        );

        // Read the file before activate.
        let before = std::fs::read(root.join("go.work")).unwrap();

        // Call activate() — the ownership guard must short-circuit; no mutation.
        GoWork.activate(&ctx).unwrap();

        // Read the file after activate.
        let after = std::fs::read(root.join("go.work")).unwrap();

        assert_eq!(
            before, after,
            "user-held go.work must be byte-for-byte unchanged after activate()"
        );

        // Confirm the file still has no rwv marker (takeover did NOT happen).
        let text = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            !text.contains("managed by repoweave"),
            "marker must NOT be injected into a user-held file: {text}"
        );
        assert!(
            text.contains("./github/acme/server"),
            "user use entry must survive unchanged: {text}"
        );
    }

    // -----------------------------------------------------------------------
    // CLEAN
    // -----------------------------------------------------------------------

    /// Given: go.work written by activate() (marker + correct use entries).
    /// Then:  verify() returns no issues (CLEAN).
    #[test]
    fn s7_go_work_doctor_clean_after_fresh_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_go_mod(root, "github/acme/server");

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        GoWork.activate(&ctx).unwrap();

        let issues = GoWork.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "verify() must return no issues for a freshly-activated go.work, got: {issues:?}"
        );
    }
}

// ===========================================================================
// doctor verify() — vscode-workspace
// ===========================================================================

mod s7_vscode_doctor {
    use super::*;
    use repoweave::integrations::VscodeWorkspace;

    // -----------------------------------------------------------------------
    // MISSING
    // -----------------------------------------------------------------------

    /// Given: No .code-workspace file.
    /// Then:  verify() reports MISSING+safe_to_fix.
    #[test]
    fn s7_vscode_doctor_missing_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = VscodeWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one MISSING issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
        assert!(
            issue.message.contains("missing"),
            "MISSING message should contain 'missing': {}",
            issue.message
        );
    }

    /// Given: MISSING .code-workspace.
    /// When:  activate() runs.
    /// Then:  file created with rwv.generated marker; verify() returns CLEAN.
    #[test]
    fn s7_vscode_doctor_missing_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: MISSING.
        let pre = VscodeWorkspace.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "expected MISSING pre-condition");
        assert!(pre[0].safe_to_fix);

        VscodeWorkspace.activate(&ctx).unwrap();

        let filepath = root.join("test-project.code-workspace");
        assert!(
            filepath.exists(),
            "code-workspace must be created after activate"
        );

        let content = std::fs::read_to_string(&filepath).unwrap();
        assert!(
            content.contains("rwv.generated"),
            "file must have rwv.generated marker after activate: {content}"
        );

        let post = VscodeWorkspace.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post:?}"
        );
    }

    // -----------------------------------------------------------------------
    // DRIFT
    // -----------------------------------------------------------------------

    /// Given: .code-workspace with rwv.generated marker but wrong primary folder.
    /// Then:  verify() reports DRIFT+safe_to_fix.
    #[test]
    fn s7_vscode_doctor_drift_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // Wrong project name in the primary folder.
        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "rwv.generated": {"managed": true, "files.exclude": []},
  "folders": [{"path": ".", "name": "old-project (primary)"}],
  "settings": {}
}
"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = VscodeWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one DRIFT issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
        assert!(
            issue.message.contains("drift"),
            "DRIFT message should contain 'drift': {}",
            issue.message
        );
    }

    /// Given: DRIFT .code-workspace.
    /// When:  activate() runs.
    /// Then:  verify() returns CLEAN.
    #[test]
    fn s7_vscode_doctor_drift_fixed_by_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "rwv.generated": {"managed": true, "files.exclude": []},
  "folders": [{"path": ".", "name": "old-project (primary)"}],
  "settings": {}
}
"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Pre-condition: DRIFT.
        let pre = VscodeWorkspace.verify(&ctx).unwrap();
        assert_eq!(pre.len(), 1, "expected DRIFT pre-condition");

        VscodeWorkspace.activate(&ctx).unwrap();

        let post = VscodeWorkspace.verify(&ctx).unwrap();
        assert!(
            post.is_empty(),
            "verify() must return no issues after activate (CLEAN), got: {post:?}"
        );
    }

    // -----------------------------------------------------------------------
    // USER-HELD
    // -----------------------------------------------------------------------

    /// Given: .code-workspace file with NO rwv.generated marker.
    /// Then:  verify() reports USER-HELD+!safe_to_fix.
    #[test]
    fn s7_vscode_doctor_user_held_reports_finding() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        // No rwv.generated marker.
        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "folders": [{"path": ".", "name": "test-project (primary)"}],
  "settings": {}
}
"#,
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = VscodeWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one USER-HELD issue, got: {issues:?}"
        );
        let issue = &issues[0];
        assert!(
            !issue.safe_to_fix,
            "USER-HELD issue must NOT be safe_to_fix"
        );
        assert!(
            issue.message.contains("NOT auto-take-over")
                || issue.message.contains("not auto")
                || issue.message.contains("unmarked"),
            "USER-HELD message must describe no-takeover: {}",
            issue.message
        );
    }

    /// Given: USER-HELD .code-workspace.
    /// When:  activate() runs.
    /// Then:  The file is byte-identical and still USER-HELD — activate never
    ///        takes the pen from a file it does not already hold.
    #[test]
    fn s7_vscode_doctor_user_held_file_unchanged_after_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let original = r#"{
  "folders": [{"path": ".", "name": "test-project (primary)"}],
  "settings": {}
}
"#;
        write_file(root, "test-project.code-workspace", original);

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let issues = VscodeWorkspace.verify(&ctx).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(!issues[0].safe_to_fix, "must be USER-HELD");

        VscodeWorkspace.activate(&ctx).unwrap();

        let after = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        assert_eq!(
            after, original,
            "activate must leave a USER-HELD file byte-identical"
        );

        // Still USER-HELD: activate did not convert doctor's finding by
        // stamping the marker.
        let post = VscodeWorkspace.verify(&ctx).unwrap();
        assert_eq!(post.len(), 1, "expected the USER-HELD finding to persist");
        assert!(
            !post[0].safe_to_fix,
            "post-activate finding must still be USER-HELD, got: {post:?}"
        );
    }

    // -----------------------------------------------------------------------
    // CLEAN
    // -----------------------------------------------------------------------

    /// Given: .code-workspace written by activate() (marker + correct primary).
    /// Then:  verify() returns no issues (CLEAN).
    #[test]
    fn s7_vscode_doctor_clean_after_fresh_activate() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        VscodeWorkspace.activate(&ctx).unwrap();

        let issues = VscodeWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "verify() must return no issues for a freshly-activated .code-workspace, got: {issues:?}"
        );
    }
}

// ===========================================================================
// vscode-workspace — container-kind-aware regeneration of the generated
// files.exclude region
// ===========================================================================
//
// The generated exclude set is derived from a disk scan, but the file holding
// it is weave-level and committed. A container that materialized only part of
// the weave computes a strictly narrower set, so a plain replace there would
// ship the narrowing back to primary as a silent loss. Primary regeneration is
// authoritative (replace); workweave regeneration is monotone (union with the
// recorded entries this container cannot observe).

mod vscode_workspace_container_kind {
    use super::*;
    use repoweave::integrations::VscodeWorkspace;

    /// A context whose disk view and container kind are both stated
    /// explicitly. The exclude set is a function of `repos_on_disk` and
    /// `project_paths`, so varying those is how these tests model a full
    /// view (primary) against a partial one (workweave); `kind` is what the
    /// integration actually branches on, and callers pass it independently
    /// of whatever `root` looks like on disk.
    #[allow(clippy::too_many_arguments)]
    fn ctx_with_view<'a>(
        root: &'a Path,
        project: &'a ProjectName,
        manifest: &'a Manifest,
        config: &'a IntegrationConfig,
        cache: &'a HashMap<String, Vec<String>>,
        repos_on_disk: &'a [RepoPath],
        project_paths: &'a [String],
        kind: ContainerKind,
    ) -> IntegrationContext<'a> {
        IntegrationContext {
            output_dir: root,
            workspace_root: root,
            container_kind: kind,
            project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config,
            all_repos_on_disk: repos_on_disk,
            all_project_paths: project_paths,
            detection_cache: cache,
            workweave: None,
        }
    }

    /// Lay down a `.rwv-workweave` marker at `root`, so the on-disk shape
    /// matches a real workweave root. The container kind fed to the
    /// integration under test comes from `ctx_with_view`'s `kind` argument,
    /// not from this file — a resolved `Checkout` is what production code
    /// threads through, and this marker is scenery for that, not the input.
    fn as_workweave_root(root: &Path) {
        write_file(
            root,
            ".rwv-workweave",
            "{\"primary\":\"/elsewhere/weave\",\"project\":\"test-project\",\"parent\":\"/elsewhere/weave\"}",
        );
    }

    /// A `.code-workspace` as a full-view container would have written it:
    /// four generated excludes, recorded in the marker, plus a user key.
    const WIDE_FILE: &str = r#"{
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "rwv.generated": {
    "managed": true,
    "files.exclude": [".*", "github/other", "github/acme/legacy", "projects/sibling"]
  },
  "settings": {
    "files.exclude": {
      ".*": true,
      "github/other": true,
      "github/acme/legacy": true,
      "projects/sibling": true,
      "**/target": true
    },
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3
  }
}
"#;

    fn parse(root: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    /// The keys the marker claims rwv owns, sorted.
    fn marker_excludes(parsed: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = parsed["rwv.generated"]["files.exclude"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        keys.sort();
        keys
    }

    /// Given: a workweave that materialized one member, regenerating a file a
    ///        full-view container wrote.
    /// Then:  every recorded entry naming a path this container does not have
    ///        survives — in the marker AND in the live exclude map.
    #[test]
    fn workweave_regen_preserves_entries_it_cannot_observe() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        as_workweave_root(root);
        write_file(root, "test-project.code-workspace", WIDE_FILE);
        std::fs::create_dir_all(root.join("github/acme/server")).unwrap();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let on_disk = vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
        let projects = vec!["test-project".to_string()];
        let ctx = ctx_with_view(
            root,
            &project,
            &manifest,
            &config,
            &cache,
            &on_disk,
            &projects,
            ContainerKind::Workweave,
        );

        VscodeWorkspace.activate(&ctx).unwrap();

        let parsed = parse(root);
        assert_eq!(
            marker_excludes(&parsed),
            vec![
                ".*",
                "github/acme/legacy",
                "github/other",
                "projects/sibling"
            ],
            "a workweave must not drop recorded entries about regions it never \
             materialized"
        );

        let exclude = &parsed["settings"]["files.exclude"];
        for key in ["github/other", "github/acme/legacy", "projects/sibling"] {
            assert_eq!(
                exclude[key],
                serde_json::Value::Bool(true),
                "preserved entry {key} must be live in the map, not only recorded"
            );
        }
        // Marker discipline is untouched: the user key still rides through.
        assert_eq!(exclude["**/target"], serde_json::Value::Bool(true));
    }

    /// Given: a file a full-view container authored, then regenerated by a
    ///        container that materialized less of the weave, nothing else
    ///        changed.
    /// Then:  byte-identical. The diff this used to produce on every partial
    ///        regeneration IS the symptom — a fixpoint has nothing to stash and
    ///        nothing to carry back.
    ///
    /// The prior state is authored by the code under test at a primary root
    /// rather than hand-written, so the comparison is against the real
    /// serialization and not a formatting accident.
    #[test]
    fn workweave_regen_with_no_member_change_is_a_fixpoint() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();

        // Full view: one member, one non-member repo, one sibling project.
        std::fs::create_dir_all(root.join("github/acme/server")).unwrap();
        std::fs::create_dir_all(root.join("github/other/thing")).unwrap();
        let wide_disk = vec![
            RepoPath::new("github/acme/server").expect("known-safe literal"),
            RepoPath::new("github/other/thing").expect("known-safe literal"),
        ];
        let wide_projects = vec!["test-project".to_string(), "sibling".to_string()];
        let primary_ctx = ctx_with_view(
            root,
            &project,
            &manifest,
            &config,
            &cache,
            &wide_disk,
            &wide_projects,
            ContainerKind::Primary,
        );
        VscodeWorkspace.activate(&primary_ctx).unwrap();
        let authored = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
        assert_eq!(
            marker_excludes(&parse(root)),
            vec![".*", "github/other", "projects/sibling"],
            "precondition: the full view authored all three entries"
        );

        // The same file, regenerated by a container holding only the member.
        as_workweave_root(root);
        std::fs::remove_dir_all(root.join("github/other")).unwrap();
        let narrow_disk = vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
        let narrow_projects = vec!["test-project".to_string()];
        let workweave_ctx = ctx_with_view(
            root,
            &project,
            &manifest,
            &config,
            &cache,
            &narrow_disk,
            &narrow_projects,
            ContainerKind::Workweave,
        );
        VscodeWorkspace.activate(&workweave_ctx).unwrap();
        let regenerated =
            std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();

        assert_eq!(
            authored, regenerated,
            "regeneration under a partial view must produce zero diff"
        );
    }

    /// Given: a workweave that HAS materialized the path an entry names, and a
    ///        manifest that now claims it.
    /// Then:  the entry is dropped. Monotonicity defers to the recorded prior
    ///        only where the container has no evidence; here it has some, and
    ///        keeping the entry would hide a member the user can see.
    #[test]
    fn workweave_regen_drops_an_entry_whose_path_it_can_see() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        as_workweave_root(root);
        write_file(root, "test-project.code-workspace", WIDE_FILE);
        std::fs::create_dir_all(root.join("github/acme/server")).unwrap();
        std::fs::create_dir_all(root.join("github/acme/legacy")).unwrap();

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Owned),
            ("github/acme/legacy", Role::Owned),
        ]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let on_disk = vec![
            RepoPath::new("github/acme/server").expect("known-safe literal"),
            RepoPath::new("github/acme/legacy").expect("known-safe literal"),
        ];
        let projects = vec!["test-project".to_string()];
        let ctx = ctx_with_view(
            root,
            &project,
            &manifest,
            &config,
            &cache,
            &on_disk,
            &projects,
            ContainerKind::Workweave,
        );

        VscodeWorkspace.activate(&ctx).unwrap();

        let parsed = parse(root);
        assert!(
            !marker_excludes(&parsed).contains(&"github/acme/legacy".to_string()),
            "an entry naming a path this container materialized is the \
             container's own business to drop: {:?}",
            marker_excludes(&parsed)
        );
        assert!(
            parsed["settings"]["files.exclude"]
                .get("github/acme/legacy")
                .is_none(),
            "the dropped entry must leave the live map too"
        );
        // The regions it still cannot observe are untouched.
        assert!(marker_excludes(&parsed).contains(&"github/other".to_string()));
    }

    /// Given: a primary root (no marker file) whose full scan says three of the
    ///        recorded entries name nothing.
    /// Then:  they are dropped. Primary sees the whole weave, so an absent path
    ///        there is genuinely dead — the replace semantics are unchanged.
    #[test]
    fn primary_regen_drops_entries_for_genuinely_absent_paths() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(root, "test-project.code-workspace", WIDE_FILE);
        std::fs::create_dir_all(root.join("github/acme/server")).unwrap();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let on_disk = vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
        let projects = vec!["test-project".to_string()];
        let ctx = ctx_with_view(
            root,
            &project,
            &manifest,
            &config,
            &cache,
            &on_disk,
            &projects,
            ContainerKind::Primary,
        );

        VscodeWorkspace.activate(&ctx).unwrap();

        let parsed = parse(root);
        assert_eq!(
            marker_excludes(&parsed),
            vec![".*"],
            "primary regeneration is authoritative: entries for paths absent \
             from its full view are dead and must go"
        );
        let exclude = &parsed["settings"]["files.exclude"];
        assert!(exclude.get("github/other").is_none());
        assert!(exclude.get("github/acme/legacy").is_none());
        assert!(exclude.get("projects/sibling").is_none());
        // Still only the generated region moves.
        assert_eq!(exclude["**/target"], serde_json::Value::Bool(true));
    }

    /// Given: a full-view file being verified from a workweave.
    /// Then:  CLEAN. The entries this container cannot observe are what a
    ///        regeneration here would keep, so they are not drift.
    #[test]
    fn verify_in_a_workweave_does_not_report_preserved_entries_as_drift() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        as_workweave_root(root);
        write_file(root, "test-project.code-workspace", WIDE_FILE);
        std::fs::create_dir_all(root.join("github/acme/server")).unwrap();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let on_disk = vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
        let projects = vec!["test-project".to_string()];
        let ctx = ctx_with_view(
            root,
            &project,
            &manifest,
            &config,
            &cache,
            &on_disk,
            &projects,
            ContainerKind::Workweave,
        );

        let issues = VscodeWorkspace.verify(&ctx).unwrap();
        assert!(
            issues.is_empty(),
            "a workweave must not call the weave's own exclude set drift: {issues:?}"
        );
    }

    /// Given: a primary root whose committed file records one exclude while its
    ///        full scan justifies two — the shrunk state a partial regeneration
    ///        used to ship here.
    /// Then:  DRIFT, safe to fix. Primary still reports what it can prove.
    #[test]
    fn verify_at_primary_reports_a_shrunk_generated_set_as_drift() {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "test-project.code-workspace",
            r#"{
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "rwv.generated": { "managed": true, "files.exclude": [".*"] },
  "settings": { "files.exclude": { ".*": true } }
}
"#,
        );
        std::fs::create_dir_all(root.join("github/acme/server")).unwrap();
        std::fs::create_dir_all(root.join("github/other/thing")).unwrap();

        let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let on_disk = vec![
            RepoPath::new("github/acme/server").expect("known-safe literal"),
            RepoPath::new("github/other/thing").expect("known-safe literal"),
        ];
        let projects = vec!["test-project".to_string()];
        let ctx = ctx_with_view(
            root,
            &project,
            &manifest,
            &config,
            &cache,
            &on_disk,
            &projects,
            ContainerKind::Primary,
        );

        let issues = VscodeWorkspace.verify(&ctx).unwrap();
        assert_eq!(
            issues.len(),
            1,
            "expected exactly one DRIFT issue, got: {issues:?}"
        );
        assert!(issues[0].safe_to_fix, "DRIFT issue must be safe_to_fix");
        assert!(
            issues[0].message.contains("drift"),
            "DRIFT message should say so: {}",
            issues[0].message
        );

        // And regeneration is what settles it.
        VscodeWorkspace.activate(&ctx).unwrap();
        assert!(
            VscodeWorkspace.verify(&ctx).unwrap().is_empty(),
            "activate must clear the drift it reported"
        );
        assert_eq!(marker_excludes(&parse(root)), vec![".*", "github/other"]);
    }
}
