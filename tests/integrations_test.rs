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
use repoweave::integration::{Integration, IntegrationContext, Severity, SurfacedFile};
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
#[cfg(unix)]
fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
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
#[cfg(unix)]
fn reports_tool_missing(report: &str, integration: &str) -> bool {
    let parsed: serde_json::Value =
        serde_json::from_str(report).expect("doctor --json emits a JSON report");
    parsed["issues"]
        .as_array()
        .expect("a doctor report carries an issues array")
        .iter()
        .any(|v| v["kind"] == "tool-missing" && v["integration"] == integration)
}

/// Assert `integration`'s `tool-missing` finding fires with its tool off the
/// child's PATH and clears with it on.
///
/// **One call, not two assertions, because the positive half is the negative
/// half's control.** A broken enumeration — the array renamed, the issue
/// channel not run, the predicate reading the wrong key — answers "not
/// reported" for every input, which is indistinguishable from the finding
/// legitimately clearing. Only the `absent` half can tell those apart, so it
/// cannot be a separate assertion someone might drop or reorder.
///
/// An emptiness check on the array used to serve this, justified by these
/// fixtures leaving managed files unwritten. That stopped being true when a
/// declared file with no source stopped drawing a surfacing finding: an empty
/// `issues` array is now a healthy report, so the control has to come from a
/// second input rather than from the shape of one.
#[cfg(unix)]
fn tool_missing_fires_then_clears(absent: &str, present: &str, integration: &str, tool: &str) {
    assert!(
        reports_tool_missing(absent, integration),
        "with {tool} off the child's PATH, doctor must raise tool-missing for \
         {integration}; got:\n{absent}"
    );
    assert!(
        !reports_tool_missing(present, integration),
        "with a {tool} on the child's PATH, the finding must clear; got:\n{present}"
    );
}

#[path = "integrations_test/activate_hooks.rs"]
mod activate_hooks;
#[path = "integrations_test/cargo_workspace.rs"]
mod cargo_workspace;
#[path = "integrations_test/gita.rs"]
mod gita;
#[path = "integrations_test/go_work.rs"]
mod go_work;
#[path = "integrations_test/npm_workspaces.rs"]
mod npm_workspaces;
#[path = "integrations_test/pnpm_workspaces.rs"]
mod pnpm_workspaces;
#[path = "integrations_test/s7_cargo_doctor.rs"]
mod s7_cargo_doctor;
#[path = "integrations_test/s7_go_work_doctor.rs"]
mod s7_go_work_doctor;
#[path = "integrations_test/s7_npm_doctor.rs"]
mod s7_npm_doctor;
#[path = "integrations_test/s7_pnpm_doctor.rs"]
mod s7_pnpm_doctor;
#[path = "integrations_test/s7_uv_doctor.rs"]
mod s7_uv_doctor;
#[path = "integrations_test/s7_vscode_doctor.rs"]
mod s7_vscode_doctor;
#[path = "integrations_test/s8_cross_port_default_only.rs"]
mod s8_cross_port_default_only;
#[path = "integrations_test/static_files.rs"]
mod static_files;
#[path = "integrations_test/uv_workspace.rs"]
mod uv_workspace;
#[path = "integrations_test/vscode_workspace.rs"]
mod vscode_workspace;
#[path = "integrations_test/vscode_workspace_container_kind.rs"]
mod vscode_workspace_container_kind;
#[path = "integrations_test/vscode_workspace_scenarios.rs"]
mod vscode_workspace_scenarios;
