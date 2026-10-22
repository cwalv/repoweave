//! An install hook runs the file its `PATH` lookup found, including through a
//! relative `PATH` entry, from a directory other than the one it was found in.
//!
//! The regression it closes: the lookup and the spawn were two resolutions. A
//! relative entry such as `tools` named `tools/npm` from the directory `rwv`
//! was run in, and the child, started in the weave root, read the same entry
//! against the weave root, found nothing, and failed the activation over a tool
//! that was installed. `rwv each` and plugin dispatch each had that bug fixed
//! for their own spawn; the install hooks spawned a bare name.
//!
//! Driven through the shipped binary, with the fake tool's marker written to an
//! absolute path so the assertion does not depend on where the child ran.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let member = ws.join("github/org/m");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(
        member.join("package.json"),
        "{\"name\": \"m\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/org/m\"]\ntype = \"git\"\nurl = \"https://github.com/org/m.git\"\n\
         version = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        &repoweave::manifest::ProjectName::new("app").unwrap(),
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation authors package.json");
    ws
}

fn path_with(first: &Path) -> OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(
        std::iter::once(first.to_path_buf()).chain(std::env::split_paths(&inherited)),
    )
    .unwrap()
}

#[test]
fn an_install_tool_on_a_relative_path_entry_runs_from_the_weave_root() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());

    let shell = tmp.path().join("shell");
    let tools = shell.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let marker = tmp.path().join("npm-ran");
    let write_marker = format!("echo ran > \"{}\"\n", marker.display());
    common::write_plugin(&tools, "npm", &write_marker, &write_marker);

    let output = common::rwv()
        .args(["-C", &ws.to_string_lossy(), "activate", "app"])
        .current_dir(&shell)
        .env("PATH", path_with(Path::new("tools")))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "an install tool found through a relative PATH entry must run; got:\n{stderr}"
    );
    assert!(
        marker.exists(),
        "the hook must have run `{}`; got:\n{stderr}",
        tools.display()
    );
}
