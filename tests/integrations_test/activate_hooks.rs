// ===========================================================================
// Integration activate hooks
// ===========================================================================
//
// Each ecosystem integration should have an `activate_hook` that runs the
// install command. Non-ecosystem integrations (gita, vscode) should have
// no-op hooks.

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
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    // No package.json in any repo
    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let integration = NpmWorkspaces;
    let result = integration.activate_hook(&ctx);
    assert!(
        result.is_ok(),
        "npm activate hook should be no-op when no repos detected"
    );
    assert!(
        !project_dir.join("package-lock.json").exists(),
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
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let integration = CargoWorkspace;
    let result = integration.activate_hook(&ctx);
    assert!(
        result.is_ok(),
        "cargo activate hook should be no-op when no repos detected"
    );
    assert!(
        !project_dir.join("Cargo.lock").exists(),
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

/// A `cargo` that replaces the lock's path rather than writing through the
/// link `surface_symlinks` put there — the shape `pnpm` and npm's
/// `write-file-atomic` take on a first generation, when the link dangles —
/// leaves a real file at the weave root with the canonical lock still missing.
/// The verb that ran the hook takes that file into the project and puts the
/// link back, instead of stopping on an "orphan".
///
/// Gated on the fixture, not the subject: a `cargo` that replaces the path on
/// cue is a shebang shim on `PATH`, which Windows will neither find nor spawn.
#[cfg(unix)]
#[test]
fn cargo_activate_hook_whose_tool_replaces_the_link_has_its_lock_adopted() {
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

    // A `cargo` that reports success after replacing whatever stands at the
    // lock's root path with a new file.
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

    assert_eq!(
        std::fs::read_to_string(ws.join("projects/app/Cargo.lock"))
            .ok()
            .as_deref(),
        Some("# fake lock\n"),
        "the lock the tool wrote must reach the project copy:\n{report}"
    );
    assert_eq!(
        std::fs::read_link(ws.join("Cargo.lock")).ok(),
        Some(std::path::PathBuf::from("projects/app/Cargo.lock")),
        "and the link must stand again:\n{report}"
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
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let integration = UvWorkspace;
    let result = integration.activate_hook(&ctx);
    assert!(
        result.is_ok(),
        "uv activate hook should be no-op when no repos detected"
    );
    assert!(
        !project_dir.join("uv.lock").exists(),
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
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true");
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let integration = PnpmWorkspaces;
    let result = integration.activate_hook(&ctx);
    assert!(
        result.is_ok(),
        "pnpm activate hook should be no-op when no repos detected"
    );
    assert!(
        !project_dir.join("pnpm-lock.yaml").exists(),
        "no pnpm-lock.yaml should be created when no repos detected"
    );
}

// -----------------------------------------------------------------------
// A hook whose tool is not on PATH
// -----------------------------------------------------------------------

/// A hook whose tool is absent from `PATH` ran nothing, so it is a
/// `tool-missing` warning rather than a failed verb, and the other
/// integrations' hooks still run. Driven through both the repair verb
/// (`doctor --fix`, which reaches the hooks only over drift) and selection.
///
/// Gated on the fixture: the `cargo` that proves the other hook ran is a
/// shebang shim on `PATH`.
#[cfg(unix)]
#[test]
fn hook_whose_tool_is_missing_warns_and_the_other_hooks_still_run() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let bin = tmp.path().join("bin");
    let witness = tmp.path().join("cargo-ran");
    std::fs::create_dir_all(ws.join("projects/app")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(which::which("git").unwrap(), bin.join("git")).unwrap();

    write_file(
        &ws,
        "github/acme/web/package.json",
        "{\"name\": \"web\", \"version\": \"0.1.0\"}\n",
    );
    write_file(
        &ws,
        "github/acme/server/Cargo.toml",
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    git_init_with_commit(&ws.join("github/acme/web"));
    git_init_with_commit(&ws.join("github/acme/server"));
    write_file(
        &ws,
        "projects/app/rwv.toml",
        "[repositories.\"github/acme/web\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/web.git\"\nversion = \"main\"\nrole = \"owned\"\n\
         [repositories.\"github/acme/server\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n\
         [integrations.pnpm-workspaces]\nenabled = true\n\
         [integrations.npm-workspaces]\nenabled = false\n\
         [integrations.vscode-workspace]\nenabled = false\n",
    );
    write_file(&ws, ".rwv-active", "app\n");

    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\nprintf '# lock\\n' > Cargo.lock\nexit 0\n",
            witness.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();

    let run = |args: &[&str]| {
        let out = common::rwv()
            .args(args)
            .current_dir(&ws)
            .env("PATH", bin.display().to_string())
            .output()
            .expect("rwv should run");
        let report = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), report)
    };
    let skipped_hook = "[warning] pnpm-workspaces: pnpm is not on PATH, so this \
                        integration's install hook did not run";

    for args in [&["doctor", "--fix"][..], &["activate", "app"][..]] {
        let _ = std::fs::remove_file(&witness);
        let (ok, report) = run(args);
        assert!(
            ok,
            "`rwv {}` must not fail over a tool that is not installed:\n{report}",
            args.join(" ")
        );
        assert!(
            report.contains(skipped_hook),
            "`rwv {}` must report the skipped hook as a tool-missing warning:\n{report}",
            args.join(" ")
        );
        assert!(
            !std::fs::read_to_string(&witness)
                .unwrap_or_default()
                .is_empty(),
            "`rwv {}` must still run the cargo hook:\n{report}",
            args.join(" ")
        );
    }
}

// -----------------------------------------------------------------------
// go-work: no activate hook (default no-op)
// -----------------------------------------------------------------------

#[test]
fn go_work_activate_hook_is_noop() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    touch(root, "github/acme/server/go.mod");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let integration = GoWork;
    let result = integration.activate_hook(&ctx);
    assert!(result.is_ok(), "go-work activate hook should be a no-op");
    assert!(
        !project_dir.join("go.work.sum").exists(),
        "go-work runs no install hook; a real hook's `go mod download` would write go.work.sum"
    );
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
    project_on_disk(root, "test-project");
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

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
    project_on_disk(root, "test-project");
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let integration = VscodeWorkspace;
    let result = integration.activate_hook(&ctx);
    assert!(
        result.is_ok(),
        "vscode-workspace activate hook should be a no-op"
    );
}
