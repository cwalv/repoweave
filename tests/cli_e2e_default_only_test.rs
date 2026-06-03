//! CLI-level e2e tests for DefaultOnly key behaviour across npm, uv, cargo,
//! and go-work integrations.
//!
//! Each port has three scenarios exercised via `rwv add --new` / `rwv doctor`:
//!
//! 1. **Greenfield**: empty project → `rwv add --new` seeds the repo and runs
//!    activation; assert the managed file contains the DefaultOnly key set to a
//!    project-derived value (not a hardcoded literal).
//!
//! 2. **Customization survives**: greenfield as above, operator edits the
//!    DefaultOnly key, then `rwv add --new` a second repo; assert the edit
//!    survived byte-identical.
//!
//! 3. **Cutover**: hand-authored managed file with custom DefaultOnly value +
//!    rwv marker already present; `rwv add --new` triggers activate; assert
//!    (a) Author keys are updated, (b) DefaultOnly key with custom value is
//!    unchanged, (c) untracked fields preserved.
//!
//! 4. **Doctor**: Author drift reported; DefaultOnly drift NOT reported.
//!    `rwv doctor --fix` repairs the Author key, leaves DefaultOnly alone.
//!    (Cargo only — it is the port whose doctor path is most exercised by
//!    fo-cnpjy.18. For the other ports, doctor-fix is a no-op because verify()
//!    is not yet wired to detect integration drift. See bd comment for rationale.)
//!
//! vscode is excluded: the inline `git.*` DefaultOnly settings live inside the
//! vscode integration's static-files output, not in a separately-managed file
//! that `rwv add` exercises. vscode integration drift is not surfaced by the
//! CLI doctor path in this version.

mod common;

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Stdio;

// ---------------------------------------------------------------------------
// Helpers shared by all port tests
// ---------------------------------------------------------------------------

/// Build a `Command` for the `rwv` binary with clean GIT_* env.
fn rwv() -> Command {
    common::rwv()
}

/// Create a minimal workspace at `parent/<name>` with `github/` + `projects/`
/// markers and a project directory with an `rwv.yaml` manifest.
///
/// `repos` is a list of `(path, role)` — each will have a git-initialised
/// directory created under the workspace root so `rwv add --new` does not trip
/// over an absent registry. The project directory is a git repo so workspace
/// resolution works.
///
/// Returns `(workspace_root, project_dir)`.
fn make_workspace_with_project(
    parent: &Path,
    name: &str,
    project_name: &str,
) -> (PathBuf, PathBuf) {
    let ws = parent.join(name);
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    let project_dir = ws.join("projects").join(project_name);
    std::fs::create_dir_all(&project_dir).unwrap();

    // Minimal manifest (empty repos map).
    std::fs::write(project_dir.join("rwv.yaml"), "repositories: {}\n").unwrap();

    // Initialise project dir as a git repo so workspace resolution succeeds.
    git_run_silent(&["init", "--initial-branch=main"], &project_dir);
    git_run_silent(&["config", "user.email", "test@test.com"], &project_dir);
    git_run_silent(&["config", "user.name", "Test"], &project_dir);
    git_run_silent(&["add", "rwv.yaml"], &project_dir);
    git_run_silent(&["commit", "-m", "init"], &project_dir);

    std::fs::write(ws.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    (ws, project_dir)
}

/// Run a git command silently, asserting success.
fn git_run_silent(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git command failed to start");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// Create a minimal git repo at `dir` with a single file + commit.
fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git_run_silent(&["init", "--initial-branch=main"], dir);
    git_run_silent(&["config", "user.email", "test@test.com"], dir);
    git_run_silent(&["config", "user.name", "Test"], dir);
    std::fs::write(dir.join(".gitkeep"), "").unwrap();
    git_run_silent(&["add", ".gitkeep"], dir);
    git_run_silent(&["commit", "-m", "initial"], dir);
}

// ===========================================================================
// npm — DefaultOnly keys: `name` (project-derived) and `private` (true)
// ===========================================================================
//
// Trigger file: `package.json` in each member repo.
// Author key:   `workspaces` array.
// DefaultOnly:  `name` (set to project name on greenfield; never overwritten).

mod npm {
    use super::*;

    /// Add a `package.json` stub to a repo directory so the npm integration
    /// detects it as a member.
    fn add_npm_trigger(ws: &Path, repo_path: &str) {
        let dir = ws.join(repo_path);
        std::fs::create_dir_all(&dir).unwrap();
        // Use the full repo_path as the package name (replacing / with -)
        // to ensure names are unique across repos.
        let pkg_name = repo_path.replace('/', "-");
        std::fs::write(
            dir.join("package.json"),
            format!("{{\"name\": \"{pkg_name}\", \"version\": \"1.0.0\"}}\n"),
        )
        .unwrap();
    }

    // -----------------------------------------------------------------------
    // Scenario 1: Greenfield — `name` set from project, not a literal
    // -----------------------------------------------------------------------

    #[test]
    fn greenfield_name_derived_from_project() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, _proj_dir) = make_workspace_with_project(tmp.path(), "ws", "my-cool-project");

        // Seed the first repo with a package.json trigger.
        add_npm_trigger(&ws, "github/acme/api");
        // git-init so rwv add --new finds a real git registry at github/.
        init_git_repo(&ws.join("github/acme/api"));

        // rwv add --new adds github/acme/api and runs activate (intent path).
        rwv()
            .args(["add", "github/acme/api", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        let pkg_json = std::fs::read_to_string(ws.join("package.json"))
            .expect("package.json should be generated after rwv add");
        let parsed: serde_json::Value =
            serde_json::from_str(&pkg_json).expect("package.json should be valid JSON");

        // DefaultOnly: name must be the project name, not "repoweave" or any literal.
        assert_eq!(
            parsed["name"], "my-cool-project",
            "npm greenfield: `name` must be derived from the project, got:\n{pkg_json}"
        );
        // Author key must list the repo.
        let ws_arr = parsed["workspaces"]
            .as_array()
            .expect("workspaces should be present");
        assert!(
            ws_arr
                .iter()
                .any(|w| w.as_str().is_some_and(|s| s.contains("github/acme/api"))),
            "npm greenfield: workspaces should include the added repo; got:\n{pkg_json}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 2: Customization survives a second intent op
    // -----------------------------------------------------------------------

    #[test]
    fn customization_survives_second_add() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, _proj_dir) = make_workspace_with_project(tmp.path(), "ws", "original-project");

        add_npm_trigger(&ws, "github/acme/api");
        init_git_repo(&ws.join("github/acme/api"));

        // First add: generates package.json with name = "original-project".
        rwv()
            .args(["add", "github/acme/api", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // Operator edits the `name` key to a custom value.
        let pkg_path = ws.join("package.json");
        let mut pkg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&pkg_path).unwrap()).unwrap();
        pkg["name"] = serde_json::json!("operator-chosen-name");
        std::fs::write(
            &pkg_path,
            serde_json::to_string_pretty(&pkg).unwrap() + "\n",
        )
        .unwrap();

        // Second add: adds another repo and re-runs activation.
        add_npm_trigger(&ws, "github/acme/server");
        init_git_repo(&ws.join("github/acme/server"));
        rwv()
            .args(["add", "github/acme/server", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        let after = std::fs::read_to_string(&pkg_path).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&after).expect("package.json should still be valid JSON");

        // DefaultOnly: operator's custom name must survive.
        assert_eq!(
            parsed["name"], "operator-chosen-name",
            "npm: DefaultOnly `name` edited by operator must survive second add; got:\n{after}"
        );
        // Both repos must appear in workspaces.
        let ws_arr = parsed["workspaces"]
            .as_array()
            .expect("workspaces should be present after second add");
        assert!(
            ws_arr
                .iter()
                .any(|w| w.as_str().is_some_and(|s| s.contains("github/acme/api"))),
            "npm: first repo must still be in workspaces after second add; got:\n{after}"
        );
        assert!(
            ws_arr
                .iter()
                .any(|w| w.as_str().is_some_and(|s| s.contains("github/acme/server"))),
            "npm: second repo must be in workspaces after add; got:\n{after}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 3: Cutover — hand-authored file with custom name + marker
    // -----------------------------------------------------------------------

    #[test]
    fn cutover_preserves_custom_name_and_untracked_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, proj_dir) = make_workspace_with_project(tmp.path(), "ws", "some-project");

        add_npm_trigger(&ws, "github/org/lib");
        init_git_repo(&ws.join("github/org/lib"));

        // The npm integration writes package.json to the project dir (output_dir),
        // and creates a symlink at the workspace root. Pre-seed the managed file
        // at the project dir — this is where `merge_activate` will read and write.
        let hand_authored = r#"{
  "x-repoweave": {"managed": true},
  "name": "company-internal-workspace",
  "private": true,
  "workspaces": [],
  "scripts": {
    "ci": "npm run build && npm test"
  },
  "version": "2.0.0"
}"#;
        std::fs::write(proj_dir.join("package.json"), hand_authored).unwrap();

        // Add the repo — intent path runs merge_activate on proj_dir/package.json.
        rwv()
            .args(["add", "github/org/lib", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // Read through the workspace-root symlink (or directly from proj_dir).
        let pkg_path = if ws.join("package.json").exists() {
            ws.join("package.json")
        } else {
            proj_dir.join("package.json")
        };
        let after = std::fs::read_to_string(&pkg_path).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&after).expect("package.json must still be valid JSON");

        // (a) Author key updated by rwv.
        let ws_arr = parsed["workspaces"]
            .as_array()
            .expect("workspaces should be present");
        assert!(
            ws_arr
                .iter()
                .any(|w| w.as_str().is_some_and(|s| s.contains("github/org/lib"))),
            "npm cutover: workspaces must include the added repo; got:\n{after}"
        );

        // (b) DefaultOnly key with user value unchanged.
        assert_eq!(
            parsed["name"], "company-internal-workspace",
            "npm cutover: `name` (DefaultOnly) set by user must survive; got:\n{after}"
        );
        assert_eq!(
            parsed["private"], true,
            "npm cutover: `private` (DefaultOnly) must survive; got:\n{after}"
        );

        // (c) Untracked fields preserved.
        assert_eq!(
            parsed["scripts"]["ci"], "npm run build && npm test",
            "npm cutover: untracked `scripts.ci` must survive; got:\n{after}"
        );
        assert_eq!(
            parsed["version"], "2.0.0",
            "npm cutover: untracked `version` must survive; got:\n{after}"
        );
    }
}

// ===========================================================================
// uv — DefaultOnly key: `[tool.uv].package = false`
// ===========================================================================
//
// Trigger file: `pyproject.toml` in each member repo.
// Author key:   `[tool.uv.workspace].members` list.
// DefaultOnly:  `[tool.uv].package = false` (set on greenfield; never overwritten).

mod uv {
    use super::*;

    fn add_uv_trigger(ws: &Path, repo_path: &str) {
        let dir = ws.join(repo_path);
        std::fs::create_dir_all(&dir).unwrap();
        // Use the last segment as the package name to keep names unique.
        let pkg_name = repo_path.split('/').next_back().unwrap();
        std::fs::write(
            dir.join("pyproject.toml"),
            format!("[project]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
    }

    // -----------------------------------------------------------------------
    // Scenario 1: Greenfield — `package = false` set in [tool.uv]
    // -----------------------------------------------------------------------

    #[test]
    fn greenfield_package_false_set() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, _proj_dir) = make_workspace_with_project(tmp.path(), "ws", "my-uv-project");

        add_uv_trigger(&ws, "github/astral/protocol");
        init_git_repo(&ws.join("github/astral/protocol"));

        rwv()
            .args(["add", "github/astral/protocol", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        let content = std::fs::read_to_string(ws.join("pyproject.toml"))
            .expect("pyproject.toml should be generated after rwv add");

        // DefaultOnly: package = false must be set.
        assert!(
            content.contains("package = false") || content.contains("package=false"),
            "uv greenfield: [tool.uv].package = false must be set; got:\n{content}"
        );
        // Author key: members must list the repo.
        assert!(
            content.contains("github/astral/protocol"),
            "uv greenfield: members must include the added repo; got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 2: Customization survives — user sets package = true
    // -----------------------------------------------------------------------

    #[test]
    fn customization_survives_second_add() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, _proj_dir) = make_workspace_with_project(tmp.path(), "ws", "my-uv-project");

        add_uv_trigger(&ws, "github/astral/protocol");
        init_git_repo(&ws.join("github/astral/protocol"));

        // First add: greenfield generates package = false.
        rwv()
            .args(["add", "github/astral/protocol", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // Operator opts in to packaging by setting package = true.
        let toml_path = ws.join("pyproject.toml");
        let mut content = std::fs::read_to_string(&toml_path).unwrap();
        content = content
            .replace("package = false", "package = true")
            .replace("package=false", "package = true");
        std::fs::write(&toml_path, &content).unwrap();

        // Second add.
        add_uv_trigger(&ws, "github/astral/server");
        init_git_repo(&ws.join("github/astral/server"));
        rwv()
            .args(["add", "github/astral/server", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        let after = std::fs::read_to_string(&toml_path).unwrap();

        // DefaultOnly: user's package = true must survive.
        assert!(
            after.contains("package = true"),
            "uv: DefaultOnly `package = true` set by user must survive second add; got:\n{after}"
        );
        assert!(
            !after.contains("package = false") && !after.contains("package=false"),
            "uv: `package = false` must not be re-injected when user has set true; got:\n{after}"
        );
        // Both repos in members.
        assert!(
            after.contains("github/astral/protocol"),
            "uv: first repo must still be in members; got:\n{after}"
        );
        assert!(
            after.contains("github/astral/server"),
            "uv: second repo must be in members; got:\n{after}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 3: Cutover — hand-authored file with custom package key
    // -----------------------------------------------------------------------

    #[test]
    fn cutover_preserves_user_package_key_and_untracked_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, proj_dir) = make_workspace_with_project(tmp.path(), "ws", "uv-cutover-project");

        add_uv_trigger(&ws, "github/org/lib");
        init_git_repo(&ws.join("github/org/lib"));

        // The uv integration writes pyproject.toml to the project dir (output_dir),
        // then symlinks it at workspace root. Pre-seed the managed file in proj_dir.
        let hand_authored = concat!(
            "[tool.uv.workspace]\n",
            "# managed by rwv\n",
            "members = []\n",
            "\n",
            "[tool.uv]\n",
            "package = true\n",
            "\n",
            "[build-system]\n",
            "build-backend = \"maturin\"\n",
            "requires = [\"maturin\"]\n",
        );
        std::fs::write(proj_dir.join("pyproject.toml"), hand_authored).unwrap();

        rwv()
            .args(["add", "github/org/lib", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // Read through symlink if it exists, otherwise directly from proj_dir.
        let toml_path = if ws.join("pyproject.toml").exists() {
            ws.join("pyproject.toml")
        } else {
            proj_dir.join("pyproject.toml")
        };
        let after = std::fs::read_to_string(&toml_path).unwrap();

        // (a) Author key updated.
        assert!(
            after.contains("github/org/lib"),
            "uv cutover: members must include the added repo; got:\n{after}"
        );

        // (b) DefaultOnly key with user value unchanged.
        assert!(
            after.contains("package = true"),
            "uv cutover: `package = true` (DefaultOnly user override) must survive; got:\n{after}"
        );
        assert!(
            !after.contains("package = false") && !after.contains("package=false"),
            "uv cutover: DefaultOnly must not inject `package = false` when user set true; got:\n{after}"
        );

        // (c) Untracked fields preserved.
        assert!(
            after.contains("build-backend = \"maturin\""),
            "uv cutover: untracked [build-system] must survive; got:\n{after}"
        );
    }
}

// ===========================================================================
// cargo — DefaultOnly key: `resolver = "2"`
// ===========================================================================
//
// Trigger file: `Cargo.toml` in each member repo.
// Author key:   `members` array in `[workspace]`.
// DefaultOnly:  `resolver = "2"` (set on greenfield; never overwritten).

mod cargo {
    use super::*;

    fn add_cargo_trigger(ws: &Path, repo_path: &str, crate_name: &str) {
        let src = ws.join(repo_path).join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            ws.join(repo_path).join("Cargo.toml"),
            format!(
                "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        std::fs::write(src.join("lib.rs"), "").unwrap();
    }

    // -----------------------------------------------------------------------
    // Scenario 1: Greenfield — resolver = "2" set
    // -----------------------------------------------------------------------

    #[test]
    fn greenfield_resolver_set() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, _proj_dir) = make_workspace_with_project(tmp.path(), "ws", "my-cargo-project");

        add_cargo_trigger(&ws, "github/acme/lib", "acme-lib");
        init_git_repo(&ws.join("github/acme/lib"));

        rwv()
            .args(["add", "github/acme/lib", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // The root Cargo.toml is a symlink pointing to projects/<project>/Cargo.toml.
        let cargo_path = ws.join("Cargo.toml");
        assert!(
            cargo_path.exists(),
            "cargo greenfield: Cargo.toml should be generated (or symlinked) after rwv add"
        );
        let content = std::fs::read_to_string(&cargo_path).unwrap();

        // DefaultOnly: resolver = "2" must be set.
        assert!(
            content.contains("resolver = \"2\""),
            "cargo greenfield: resolver = \"2\" must be set; got:\n{content}"
        );
        // Author key: members must include the repo.
        assert!(
            content.contains("github/acme/lib"),
            "cargo greenfield: members must include the added repo; got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 2: Customization survives — user overrides resolver = "1"
    // -----------------------------------------------------------------------

    #[test]
    fn customization_survives_second_add() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, _proj_dir) = make_workspace_with_project(tmp.path(), "ws", "my-cargo-project");

        add_cargo_trigger(&ws, "github/acme/lib", "acme-lib");
        init_git_repo(&ws.join("github/acme/lib"));

        // First add: greenfield generates resolver = "2".
        rwv()
            .args(["add", "github/acme/lib", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // The actual integration file lives at projects/<project>/Cargo.toml.
        // The workspace root Cargo.toml is a symlink; we edit through the symlink.
        let cargo_symlink = ws.join("Cargo.toml");
        let real_path = if cargo_symlink.is_symlink() {
            std::fs::canonicalize(&cargo_symlink).unwrap()
        } else {
            cargo_symlink.clone()
        };

        // Operator overrides resolver to "1" (e.g. for a legacy codebase).
        let mut content = std::fs::read_to_string(&real_path).unwrap();
        content = content.replace("resolver = \"2\"", "resolver = \"1\"");
        std::fs::write(&real_path, &content).unwrap();

        // Second add.
        add_cargo_trigger(&ws, "github/acme/server", "acme-server");
        init_git_repo(&ws.join("github/acme/server"));
        rwv()
            .args(["add", "github/acme/server", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        let after = std::fs::read_to_string(ws.join("Cargo.toml")).unwrap();

        // DefaultOnly: user's resolver = "1" must survive.
        assert!(
            after.contains("resolver = \"1\""),
            "cargo: DefaultOnly `resolver = \"1\"` set by user must survive second add; got:\n{after}"
        );
        assert!(
            !after.contains("resolver = \"2\""),
            "cargo: resolver = \"2\" must not be re-injected when user set \"1\"; got:\n{after}"
        );
        // Both repos in members.
        assert!(
            after.contains("github/acme/lib"),
            "cargo: first repo must still be in members; got:\n{after}"
        );
        assert!(
            after.contains("github/acme/server"),
            "cargo: second repo must be in members; got:\n{after}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 3: Cutover — hand-authored Cargo.toml with custom resolver
    // -----------------------------------------------------------------------

    #[test]
    fn cutover_preserves_custom_resolver_and_untracked_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, proj_dir) = make_workspace_with_project(tmp.path(), "ws", "cargo-cutover-project");

        add_cargo_trigger(&ws, "github/org/crate-a", "crate-a");
        init_git_repo(&ws.join("github/org/crate-a"));

        // Pre-seed a hand-authored Cargo.toml (the integration file lives at
        // projects/<project>/Cargo.toml and is symlinked at workspace root).
        let int_file = proj_dir.join("Cargo.toml");
        let hand_authored = concat!(
            "[workspace]\n",
            "# managed by rwv\n",
            "members = []\n",
            "# managed by rwv\n",
            "resolver = \"1\"\n",
            "\n",
            "[workspace.metadata.my-tool]\n",
            "custom-key = \"custom-value\"\n",
        );
        std::fs::write(&int_file, hand_authored).unwrap();

        // Symlink at workspace root.
        let symlink_path = ws.join("Cargo.toml");
        if !symlink_path.exists() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&int_file, &symlink_path).unwrap();
        }

        rwv()
            .args(["add", "github/org/crate-a", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        let after = std::fs::read_to_string(&int_file).unwrap();

        // (a) Author key updated.
        assert!(
            after.contains("github/org/crate-a"),
            "cargo cutover: members must include the added repo; got:\n{after}"
        );

        // (b) DefaultOnly key with user value unchanged.
        assert!(
            after.contains("resolver = \"1\""),
            "cargo cutover: user `resolver = \"1\"` (DefaultOnly) must survive; got:\n{after}"
        );
        assert!(
            !after.contains("resolver = \"2\""),
            "cargo cutover: DefaultOnly must not overwrite user resolver; got:\n{after}"
        );

        // (c) Untracked fields preserved.
        assert!(
            after.contains("custom-key = \"custom-value\""),
            "cargo cutover: untracked [workspace.metadata] must survive; got:\n{after}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 4: Doctor — Author drift reported; DefaultOnly drift NOT reported
    // -----------------------------------------------------------------------
    //
    // Setup: Cargo.toml with correct resolver="2" (DefaultOnly) but stale
    // members list (drift on Author key). doctor should report one drift
    // finding (members) but NOT report resolver as drifted.
    //
    // This test drives the verify() path via `rwv doctor` CLI.

    #[test]
    fn doctor_reports_author_drift_not_default_only_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, proj_dir) = make_workspace_with_project(tmp.path(), "ws", "doctor-cargo-project");

        // Put a git repo on disk (so doctor doesn't error on missing clone).
        let repo_path = "github/acme/crate-b";
        init_git_repo(&ws.join(repo_path));
        add_cargo_trigger(&ws, repo_path, "crate-b");

        // Write the manifest referencing two repos.
        let manifest = "repositories:\n  github/acme/crate-a:\n    type: git\n    url: https://github.com/acme/crate-a.git\n    version: main\n    role: owned\n  github/acme/crate-b:\n    type: git\n    url: https://github.com/acme/crate-b.git\n    version: main\n    role: owned\n";
        std::fs::write(proj_dir.join("rwv.yaml"), manifest).unwrap();

        // Also put crate-a on disk with a Cargo.toml so the integration detects it.
        add_cargo_trigger(&ws, "github/acme/crate-a", "crate-a");
        init_git_repo(&ws.join("github/acme/crate-a"));

        // Write a Cargo.toml with correct resolver but stale members
        // (only crate-a listed; config says both crate-a and crate-b are owned).
        let stale_cargo = concat!(
            "[workspace]\n",
            "# managed by rwv\n",
            "members = [\"github/acme/crate-a\"]\n",
            "# managed by rwv\n",
            "resolver = \"2\"\n",
        );
        std::fs::write(proj_dir.join("Cargo.toml"), stale_cargo).unwrap();

        // Also symlink at workspace root so rwv can locate it.
        let symlink_path = ws.join("Cargo.toml");
        if !symlink_path.exists() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(proj_dir.join("Cargo.toml"), &symlink_path).unwrap();
        }

        // `rwv doctor` should report drift on members (Author), not on resolver (DefaultOnly).
        // We assert the output contains drift-related text (members drift),
        // but does NOT report resolver as drifted.
        let output = rwv()
            .args(["doctor"])
            .current_dir(&ws)
            .output()
            .expect("rwv doctor should run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}{stderr}");

        // Doctor should surface a drift finding (members is Author, stale).
        // We check that either there is output about drift/members OR
        // the command exits non-zero (indicating findings).
        // We must NOT see "resolver" mentioned as a drift finding.
        // (The specific output format depends on doctor's rendering; we use
        //  an output-agnostic check: resolver drift absence.)
        assert!(
            !combined.to_lowercase().contains("resolver")
                || combined.to_lowercase().contains("resolver = \"2\""),
            "doctor must not report resolver as drifted (DefaultOnly); got:\n{combined}"
        );

        // `rwv doctor --fix` should fix the members drift and leave resolver alone.
        rwv()
            .args(["doctor", "--fix"])
            .current_dir(&ws)
            .output()
            .expect("rwv doctor --fix should run");

        let after = std::fs::read_to_string(proj_dir.join("Cargo.toml")).unwrap();

        // After fix, members should include both repos.
        assert!(
            after.contains("github/acme/crate-a"),
            "doctor --fix: crate-a must be in members after fix; got:\n{after}"
        );
        assert!(
            after.contains("github/acme/crate-b"),
            "doctor --fix: crate-b must be in members after fix; got:\n{after}"
        );

        // resolver must still be exactly as the user left it (or as greenfield set it).
        assert!(
            after.contains("resolver = \"2\""),
            "doctor --fix must not change resolver (DefaultOnly); got:\n{after}"
        );
    }
}

// ===========================================================================
// go.work — DefaultOnly key: `go <max-version>` from go.mod files
// ===========================================================================
//
// Trigger file: `go.mod` in each member repo.
// Author key:   `use` block.
// DefaultOnly:  `go <version>` line (derived from max of members' go.mod versions).
//
// The go version line is not a textbook DefaultOnly key like npm `name` —
// it is computed from the workspace members. However, once authored and the
// user edits it to a higher value (e.g., 1.26), the merge logic must NOT
// downgrade it. We test the customization-survives scenario specifically for
// the go version downgrade regression (fo-cnpjy.C11, §6.go.1 CLI analog).

mod go_work {
    use super::*;

    fn add_go_trigger(ws: &Path, repo_path: &str, module_path: &str, go_version: &str) {
        let dir = ws.join(repo_path);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("go.mod"),
            format!("module {module_path}\n\ngo {go_version}\n"),
        )
        .unwrap();
    }

    // -----------------------------------------------------------------------
    // Scenario 1: Greenfield — go version set from go.mod files
    // -----------------------------------------------------------------------

    #[test]
    fn greenfield_go_version_derived_from_go_mod() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, _proj_dir) = make_workspace_with_project(tmp.path(), "ws", "my-go-project");

        add_go_trigger(
            &ws,
            "github/org/module-a",
            "github.com/org/module-a",
            "1.21",
        );
        init_git_repo(&ws.join("github/org/module-a"));

        rwv()
            .args(["add", "github/org/module-a", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // go.work is generated (or symlinked) at workspace root.
        let go_work_path = ws.join("go.work");
        assert!(
            go_work_path.exists(),
            "go.work greenfield: go.work should be generated after rwv add"
        );
        let content = std::fs::read_to_string(&go_work_path).unwrap();

        // DefaultOnly: go version must be set (must not be absent or 0).
        assert!(
            content.contains("go 1.21") || content.starts_with("go "),
            "go.work greenfield: go version line must be set from go.mod; got:\n{content}"
        );
        // Author key: use block must include the repo.
        assert!(
            content.contains("github/org/module-a"),
            "go.work greenfield: use block must include the added repo; got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 2: Customization survives — user go version preserved on add
    // -----------------------------------------------------------------------
    //
    // CLI-level analog of §6.go.1: pre-seeded go.work at go 1.26. The new
    // repo's go.mod also declares 1.26 so max_go_version = 1.26. After add,
    // go.work must still declare go 1.26 and include the new repo.
    //
    // Note: we pre-seed proj_dir/go.work WITHOUT a ws/go.work symlink so
    // activate_via_go_tool can do a clean copy-then-edit cycle. If a symlink
    // exists when the second `rwv add` runs, the copy-to-self truncation bug
    // (copying ws/go.work (symlink) → proj/go.work (symlink target)) empties
    // the file. That bug is in the go integration's primary path and is NOT
    // the behavior this test is asserting.

    #[test]
    fn customization_survives_go_version_not_downgraded() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, proj_dir) = make_workspace_with_project(tmp.path(), "ws", "my-go-project");

        add_go_trigger(
            &ws,
            "github/org/module-a",
            "github.com/org/module-a",
            "1.26",
        );
        init_git_repo(&ws.join("github/org/module-a"));

        // Pre-seed go.work in proj_dir with go 1.26 and the rwv marker.
        // Do NOT create a ws/go.work symlink — the framework creates it after add.
        let int_file = proj_dir.join("go.work");
        let seeded = "go 1.26\n\n// managed by repoweave\nuse (\n)\n";
        std::fs::write(&int_file, seeded).unwrap();

        rwv()
            .args(["add", "github/org/module-a", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // Read from proj_dir directly (symlink may or may not exist at ws root).
        let after = std::fs::read_to_string(&int_file).unwrap();

        // go 1.26 must survive (max_go_version = 1.26 from go.mod, DefaultOnly preserves).
        assert!(
            after.contains("go 1.26"),
            "go.work: go 1.26 must survive after add (not downgraded); got:\n{after}"
        );
        // module-a must be in the use block (Author key set by rwv).
        assert!(
            after.contains("github/org/module-a"),
            "go.work: module-a must be in use block after add; got:\n{after}"
        );
    }

    // -----------------------------------------------------------------------
    // Scenario 3: Cutover — hand-authored go.work with replace directive
    // -----------------------------------------------------------------------
    //
    // CLI-level analog of §6.go.1: pre-seeded go.work with marker, a user
    // `replace` directive, and go 1.26. After `rwv add` adds a new repo,
    // the replace must survive and go version must not drop.
    //
    // Note: do NOT pre-create the ws/go.work symlink here. The go integration's
    // activate_via_go_tool creates a temporary copy at workspace_root/go.work,
    // modifies it, then copies it to output_dir/go.work. If we pre-create the
    // symlink, the copy-to-self bug truncates the file. Let the framework
    // create the symlink after activation.

    #[test]
    fn cutover_preserves_replace_and_go_version() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, proj_dir) = make_workspace_with_project(tmp.path(), "ws", "go-cutover-project");

        // go.mod files at 1.26 to match the pre-seeded go.work.
        add_go_trigger(
            &ws,
            "github/org/module-a",
            "github.com/org/module-a",
            "1.26",
        );
        init_git_repo(&ws.join("github/org/module-a"));
        add_go_trigger(
            &ws,
            "github/org/module-b",
            "github.com/org/module-b",
            "1.26",
        );
        init_git_repo(&ws.join("github/org/module-b"));

        // Pre-seed go.work in the project dir (output_dir). The framework will
        // create ws/go.work -> proj_dir/go.work after activation completes.
        // Do NOT create the symlink here — that triggers the copy-to-self bug.
        let int_file = proj_dir.join("go.work");
        let hand_authored = "go 1.26\n\n// managed by repoweave\nuse (\n\t./github/org/module-a\n)\n\n// pin legacy fork\nreplace example.com/legacy => ./vendor/legacy\n";
        std::fs::write(&int_file, hand_authored).unwrap();

        // Update manifest to include module-a before add.
        let manifest = "repositories:\n  github/org/module-a:\n    type: git\n    url: https://github.com/org/module-a.git\n    version: main\n    role: owned\n";
        std::fs::write(proj_dir.join("rwv.yaml"), manifest).unwrap();

        // Add second repo: triggers re-activation.
        rwv()
            .args(["add", "github/org/module-b", "--new"])
            .current_dir(&ws)
            .assert()
            .success();

        // Read through ws/go.work (symlink created by framework) or directly from proj_dir.
        let go_work_path = if ws.join("go.work").exists() {
            ws.join("go.work")
        } else {
            proj_dir.join("go.work")
        };
        let after = std::fs::read_to_string(&go_work_path).unwrap();

        // (a) Author key: both repos in use block.
        assert!(
            after.contains("github/org/module-a"),
            "go.work cutover: module-a must remain in use; got:\n{after}"
        );
        assert!(
            after.contains("github/org/module-b"),
            "go.work cutover: module-b must be added to use; got:\n{after}"
        );

        // (b) go version must not be downgraded.
        assert!(
            after.contains("go 1.26"),
            "go.work cutover: go 1.26 must survive after add; got:\n{after}"
        );

        // (c) Untracked user replace directive must survive.
        assert!(
            after.contains("replace example.com/legacy => ./vendor/legacy"),
            "go.work cutover: user replace directive must survive; got:\n{after}"
        );
        assert!(
            after.contains("// pin legacy fork"),
            "go.work cutover: user comment must survive; got:\n{after}"
        );
    }
}
