//! E2E integration test for Go workspace wiring via the go-work integration.
//!
//! This test verifies that activating a project with two Go modules generates
//! a `go.work` file at the weave root that correctly wires the cross-module
//! import so that `go build ./...` succeeds.
//!
//! Requires `go` on PATH — the test skips gracefully if it is not available.

use std::process::Command;

mod common;

/// Return early (skip) if `go` is not on PATH.
macro_rules! require_go {
    () => {
        if which::which("go").is_ok() {
            // go is available, continue
        } else {
            eprintln!("skipping test: `go` not found on PATH");
            return;
        }
    };
}

#[test]
fn go_workspace_wiring_resolves_cross_module_import() {
    require_go!();

    // ------------------------------------------------------------------
    // 1. Create weave root with the required directory structure
    // ------------------------------------------------------------------
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Create registry and projects directories (workspace markers)
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    // ------------------------------------------------------------------
    // 2. Set up github/chatly/protocol — a Go module exporting a function
    // ------------------------------------------------------------------
    let protocol_dir = root.join("github/chatly/protocol");
    std::fs::create_dir_all(&protocol_dir).unwrap();

    // git init so scan_repos_on_disk recognises it as a repo
    common::git()
        .args(["init", "-q"])
        .current_dir(&protocol_dir)
        .status()
        .expect("git init protocol");

    std::fs::write(
        protocol_dir.join("go.mod"),
        "module github.com/chatly/protocol\n\ngo 1.21\n",
    )
    .unwrap();

    std::fs::write(
        protocol_dir.join("protocol.go"),
        r#"package protocol

// Greeting returns a greeting string.
func Greeting() string {
    return "hello from protocol"
}
"#,
    )
    .unwrap();

    // ------------------------------------------------------------------
    // 3. Set up github/chatly/server — imports github.com/chatly/protocol
    // ------------------------------------------------------------------
    let server_dir = root.join("github/chatly/server");
    std::fs::create_dir_all(&server_dir).unwrap();

    common::git()
        .args(["init", "-q"])
        .current_dir(&server_dir)
        .status()
        .expect("git init server");

    std::fs::write(
        server_dir.join("go.mod"),
        "module github.com/chatly/server\n\ngo 1.21\n\nrequire github.com/chatly/protocol v0.0.0\n",
    )
    .unwrap();

    std::fs::write(
        server_dir.join("server.go"),
        r#"package server

import "github.com/chatly/protocol"

// Message returns a server message using the protocol package.
func Message() string {
    return protocol.Greeting()
}
"#,
    )
    .unwrap();

    // ------------------------------------------------------------------
    // 4. Create projects/web-app/rwv.toml listing both repos as primary
    // ------------------------------------------------------------------
    let project_dir = root.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let rwv_yaml = "[repositories.\"github/chatly/protocol\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/protocol.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"github/chatly/server\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/server.git\"\nversion = \"main\"\nrole = \"owned\"\n";

    std::fs::write(project_dir.join("rwv.toml"), rwv_yaml).unwrap();

    // ------------------------------------------------------------------
    // 5. Write .rwv-active so the workspace knows which project is active
    // ------------------------------------------------------------------
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    // ------------------------------------------------------------------
    // 6. Call activate("web-app", root) via the repoweave library
    // ------------------------------------------------------------------
    {
        let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(root, None).unwrap();
        repoweave::activate::activate_intent("web-app", &ctx).expect("activate should succeed");
    }

    // ------------------------------------------------------------------
    // 7. Verify go.work exists at the weave root and lists both modules
    // ------------------------------------------------------------------
    let go_work_path = root.join("go.work");
    assert!(
        go_work_path.exists(),
        "go.work should be created at the weave root after activation"
    );

    // The symlink at root points to projects/web-app/go.work; follow it.
    let go_work_content = std::fs::read_to_string(&go_work_path).unwrap();
    assert!(
        go_work_content.contains("github/chatly/protocol"),
        "go.work should contain the protocol module path, got:\n{go_work_content}"
    );
    assert!(
        go_work_content.contains("github/chatly/server"),
        "go.work should contain the server module path, got:\n{go_work_content}"
    );

    // ------------------------------------------------------------------
    // 8. Run `go build ./...` from the server module directory.
    //
    // Go workspaces work by placing `go.work` at the repo root; `go` discovers
    // it by walking up from CWD. Running `go build ./...` inside the server
    // module directory exercises the real cross-module import resolution: the
    // compiler must locate `github.com/chatly/protocol` via the workspace
    // rather than the network.
    // ------------------------------------------------------------------
    let build_output = Command::new("go")
        .args(["build", "./..."])
        .current_dir(root.join("github/chatly/server"))
        .output()
        .expect("failed to run `go build ./...` in server module");

    let stdout = String::from_utf8_lossy(&build_output.stdout);
    let stderr = String::from_utf8_lossy(&build_output.stderr);

    assert!(
        build_output.status.success(),
        "`go build ./...` failed in server module — the go.work workspace wiring did not resolve the cross-module import.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Regression: on the primary path (real `go` on PATH), activate() must not
/// touch an existing go-line via `go work edit -go=<v>`. Before the fix,
/// `go_version_override` (here: max_go_version across members) was passed to
/// `go work edit -go=<v>` unconditionally, silently downgrading a go.work
/// whose go-line is higher than every member's go.mod.
#[test]
fn go_primary_path_preserves_existing_go_line_no_downgrade() {
    require_go!();

    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    // A single Go module whose go.mod pins a lower version than the
    // pre-existing go.work below.
    let protocol_dir = root.join("github/chatly/protocol");
    std::fs::create_dir_all(&protocol_dir).unwrap();
    common::git()
        .args(["init", "-q"])
        .current_dir(&protocol_dir)
        .status()
        .expect("git init protocol");
    std::fs::write(
        protocol_dir.join("go.mod"),
        "module github.com/chatly/protocol\n\ngo 1.20\n",
    )
    .unwrap();

    let project_dir = root.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let rwv_yaml = "[repositories.\"github/chatly/protocol\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/protocol.git\"\nversion = \"main\"\nrole = \"owned\"\n";
    std::fs::write(project_dir.join("rwv.toml"), rwv_yaml).unwrap();

    // Pre-seed go.work with a go-line ABOVE the max across members
    // (1.21 > 1.20), already carrying the ownership marker so activate()
    // takes the normal (non-user-held) path.
    //
    // 1.21 is the ceiling, not a free choice: `go work use` runs against this
    // file, and a go-line above the installed toolchain makes that command
    // download a toolchain. 1.21 is the oldest go release with that machinery,
    // so nothing able to switch can be asked to.
    std::fs::write(
        project_dir.join("go.work"),
        "go 1.21\n\n// managed by repoweave\nuse (\n\t./github/chatly/protocol\n)\n",
    )
    .unwrap();

    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();
    {
        let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(root, None).unwrap();
        repoweave::activate::activate_intent("web-app", &ctx).expect("activate should succeed");
    }

    let go_work_content = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(
        go_work_content.contains("go 1.21"),
        "existing go-line must survive on the primary path, got:\n{go_work_content}"
    );
    assert!(
        !go_work_content.contains("go 1.20"),
        "must not downgrade the go-line to the member max, got:\n{go_work_content}"
    );
}

/// Validate the "release version pin" workflow:
///
/// After developing with go.work (workspace wiring), you can switch to
/// published version pins for release. The rwv.lock records the tag name
/// when HEAD is tagged — this is the bridge between "what we tested in the
/// workspace" and "what version to publish against."
#[test]
fn go_release_version_pin_workflow() {
    require_go!();

    // ------------------------------------------------------------------
    // 1. Create weave root with the required directory structure
    // ------------------------------------------------------------------
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    // ------------------------------------------------------------------
    // 2. Set up github/chatly/protocol — a Go module exporting a function
    // ------------------------------------------------------------------
    let protocol_dir = root.join("github/chatly/protocol");
    std::fs::create_dir_all(&protocol_dir).unwrap();

    common::git_in(&protocol_dir, &["init", "-q", "-b", "main"]);

    std::fs::write(
        protocol_dir.join("go.mod"),
        "module github.com/chatly/protocol\n\ngo 1.21\n",
    )
    .unwrap();

    std::fs::write(
        protocol_dir.join("protocol.go"),
        r#"package protocol

// Greeting returns a greeting string.
func Greeting() string {
    return "hello from protocol"
}
"#,
    )
    .unwrap();

    // Commit so HEAD exists and the repo is clean
    common::git_in(&protocol_dir, &["add", "."]);
    common::git_in(&protocol_dir, &["commit", "-m", "initial protocol"]);

    // ------------------------------------------------------------------
    // 3. Set up github/chatly/server — imports github.com/chatly/protocol
    // ------------------------------------------------------------------
    let server_dir = root.join("github/chatly/server");
    std::fs::create_dir_all(&server_dir).unwrap();

    common::git_in(&server_dir, &["init", "-q", "-b", "main"]);

    std::fs::write(
        server_dir.join("go.mod"),
        "module github.com/chatly/server\n\ngo 1.21\n\nrequire github.com/chatly/protocol v0.0.0\n",
    )
    .unwrap();

    std::fs::write(
        server_dir.join("server.go"),
        r#"package server

import "github.com/chatly/protocol"

// Message returns a server message using the protocol package.
func Message() string {
    return protocol.Greeting()
}
"#,
    )
    .unwrap();

    // Commit so HEAD exists and the repo is clean
    common::git_in(&server_dir, &["add", "."]);
    common::git_in(&server_dir, &["commit", "-m", "initial server"]);

    // ------------------------------------------------------------------
    // 4. Create projects/web-app/rwv.toml listing both repos as primary
    // ------------------------------------------------------------------
    let project_dir = root.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let rwv_yaml = "[repositories.\"github/chatly/protocol\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/protocol.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"github/chatly/server\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/server.git\"\nversion = \"main\"\nrole = \"owned\"\n";

    std::fs::write(project_dir.join("rwv.toml"), rwv_yaml).unwrap();

    // ------------------------------------------------------------------
    // 5. Write .rwv-active and activate to generate go.work
    // ------------------------------------------------------------------
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();
    {
        let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(root, None).unwrap();
        repoweave::activate::activate_intent("web-app", &ctx).expect("activate should succeed");
    }

    let go_work_path = root.join("go.work");
    assert!(
        go_work_path.exists(),
        "go.work should exist after activation"
    );

    // ------------------------------------------------------------------
    // 6. Tag protocol at v1.0.0 (the "release" tag)
    // ------------------------------------------------------------------
    common::git_in(&protocol_dir, &["tag", "v1.0.0"]);

    // ------------------------------------------------------------------
    // 7. Verify go build works WITH go.work (workspace/dev mode — baseline)
    // ------------------------------------------------------------------
    let build_with_workspace = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&server_dir)
        .output()
        .expect("failed to spawn `go build ./...` (workspace mode)");

    let ws_stdout = String::from_utf8_lossy(&build_with_workspace.stdout);
    let ws_stderr = String::from_utf8_lossy(&build_with_workspace.stderr);
    assert!(
        build_with_workspace.status.success(),
        "`go build ./...` (workspace mode) failed — baseline should pass.\nstdout: {ws_stdout}\nstderr: {ws_stderr}"
    );

    // ------------------------------------------------------------------
    // 8. Remove the go.work symlink — simulate switching to release mode
    // ------------------------------------------------------------------
    std::fs::remove_file(&go_work_path).expect("failed to remove go.work symlink at weave root");

    // ------------------------------------------------------------------
    // 9. Pin server's go.mod to protocol v1.0.0 via a local replace
    //    directive (since this module is not on a Go proxy).
    // ------------------------------------------------------------------
    let mod_edit = Command::new("go")
        .args([
            "mod",
            "edit",
            "-replace",
            "github.com/chatly/protocol=../protocol",
        ])
        .current_dir(&server_dir)
        .output()
        .expect("failed to spawn `go mod edit -replace`");

    assert!(
        mod_edit.status.success(),
        "`go mod edit -replace` failed: {}",
        String::from_utf8_lossy(&mod_edit.stderr)
    );

    // ------------------------------------------------------------------
    // 10. Verify go build works WITHOUT go.work (release/replace mode)
    // ------------------------------------------------------------------
    let build_without_workspace = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&server_dir)
        .env_remove("GOWORK") // ensure no ambient go.work override
        .output()
        .expect("failed to spawn `go build ./...` (replace mode)");

    let rel_stdout = String::from_utf8_lossy(&build_without_workspace.stdout);
    let rel_stderr = String::from_utf8_lossy(&build_without_workspace.stderr);
    assert!(
        build_without_workspace.status.success(),
        "`go build ./...` (replace/release mode) failed — replace directive should resolve the dependency.\nstdout: {rel_stdout}\nstderr: {rel_stderr}"
    );

    // ------------------------------------------------------------------
    // 11. Generate rwv.lock and verify protocol's version is "v1.0.0"
    //     (HEAD is tagged, so generate_lock should record the tag name)
    // ------------------------------------------------------------------
    let manifest = repoweave::manifest::Manifest::from_path(&project_dir.join("rwv.toml"))
        .expect("failed to parse rwv.toml");

    let lock = repoweave::lock::generate_lock(&manifest, root, None, true)
        .expect("generate_lock should succeed");

    let protocol_entry = lock
        .get_entry(
            &repoweave::manifest::RepoPath::new("github/chatly/protocol")
                .expect("known-safe literal"),
        )
        .expect("lock should contain github/chatly/protocol");

    // Tag-form is preserved as the display form (round-trips into rwv.lock as
    // `version: v1.0.0`); `as_str` returns the canonical SHA.
    assert_eq!(
        protocol_entry.version.display_str(),
        "v1.0.0",
        "rwv.lock should record the tag name 'v1.0.0' for the protocol repo when HEAD is tagged"
    );
}

/// The file go-work declares under `generated_files()` must be one the `go`
/// tool actually writes through the weave-root link — `go.work.sum`, not
/// `go.sum`, which a Go workspace root never carries (a `go.sum` belongs
/// beside a member's `go.mod`). Reproduces the write-through mechanism end to
/// end: activation leaves a dangling `go.work.sum` link because nothing has
/// resolved a module yet, and `go mod download` at the weave root must write
/// through that link rather than dropping an untracked real file at the root.
#[test]
fn go_mod_download_writes_through_the_declared_go_work_sum_link() {
    require_go!();

    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    // A real external dependency: `go mod download` writes nothing at all for
    // a module with no requirements to resolve.
    let svc_dir = root.join("github/acme/svc");
    std::fs::create_dir_all(&svc_dir).unwrap();
    common::git_in(&svc_dir, &["init", "-q", "-b", "main"]);
    std::fs::write(
        svc_dir.join("go.mod"),
        "module example.com/svc\n\ngo 1.21\n\nrequire rsc.io/quote v1.5.2\n",
    )
    .unwrap();
    std::fs::write(
        svc_dir.join("main.go"),
        "package main\n\nimport (\n\t\"fmt\"\n\n\t\"rsc.io/quote\"\n)\n\nfunc main() {\n\tfmt.Println(quote.Hello())\n}\n",
    )
    .unwrap();

    let project_dir = root.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let rwv_toml = "[repositories.\"github/acme/svc\"]\ntype = \"git\"\nurl = \"https://example.invalid/acme/svc.git\"\nversion = \"main\"\nrole = \"owned\"\n";
    std::fs::write(project_dir.join("rwv.toml"), rwv_toml).unwrap();

    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();
    {
        let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(root, None).unwrap();
        repoweave::activate::activate_intent("web-app", &ctx).expect("activate should succeed");
    }

    let root_link = root.join("go.work.sum");
    let link_meta = root_link
        .symlink_metadata()
        .expect("activation should declare go.work.sum and surface a link for it");
    assert!(
        link_meta.file_type().is_symlink(),
        "the weave-root entry must be a link, not a real file, going into the \
         download below"
    );
    assert!(
        !project_dir.join("go.work.sum").exists(),
        "nothing has resolved a module yet, so the link this fixture built \
         must still be dangling — otherwise the download below proves nothing"
    );

    let download = Command::new("go")
        .args(["mod", "download"])
        .current_dir(root)
        .output()
        .expect("failed to run `go mod download` at the weave root");
    assert!(
        download.status.success(),
        "`go mod download` failed at the weave root:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&download.stdout),
        String::from_utf8_lossy(&download.stderr)
    );

    let root_meta_after = root_link
        .symlink_metadata()
        .expect("go.work.sum must still exist at the weave root after download");
    assert!(
        root_meta_after.file_type().is_symlink(),
        "go mod download must write THROUGH the declared link, not drop an \
         untracked real file at the weave root — the exact hazard a wrong \
         declaration produces"
    );

    let project_sum = project_dir.join("go.work.sum");
    assert!(
        project_sum.exists(),
        "the write should have landed at the canonical path the link routes \
         to: {}",
        project_sum.display()
    );
    let contents = std::fs::read_to_string(&project_sum).unwrap();
    assert!(
        contents.contains("rsc.io/quote"),
        "go.work.sum should record the checksum of the resolved dependency, \
         got:\n{contents}"
    );

    assert!(
        !root.join("go.sum").exists(),
        "go.sum must never appear at a workspace root; go-work declares \
         go.work.sum precisely because nothing ever writes a go.sum there"
    );
}
