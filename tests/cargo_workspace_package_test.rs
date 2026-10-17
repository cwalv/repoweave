//! `workspace-package = true` makes cargo-workspace author a
//! `[workspace.package]` table into the weave's Cargo.toml. Every authoring
//! pass must leave that file parseable, with rwv's marker on the line above
//! the table header.
//!
//! The regression pinned here is a marker inside the header's brackets,
//! `[# managed by rwv\nworkspace.package]`: an intent verb failed and rolled
//! back over it, and `doctor --fix` left it for every later verb to fail on.

use std::path::{Path, PathBuf};

mod common;

macro_rules! require_cargo {
    () => {
        if common::skip_without_tool("cargo") {
            return;
        }
    };
}

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

fn rwv(args: &[&str], cwd: &Path) -> (bool, String) {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    (
        output.status.success(),
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// Two cargo members and `workspace-package = true`, nothing authored yet.
fn weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let mut manifest = String::new();
    for crate_name in ["alpha", "beta"] {
        let member = ws.join("github/acme").join(crate_name);
        std::fs::create_dir_all(member.join("src")).unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        std::fs::write(member.join("src/lib.rs"), "").unwrap();
        git_init_with_commit(&member);
        manifest.push_str(&format!(
            "[repositories.\"github/acme/{crate_name}\"]\n\
             type = \"git\"\n\
             url = \"https://github.com/acme/{crate_name}.git\"\n\
             version = \"main\"\n\
             role = \"owned\"\n\n"
        ));
    }
    manifest.push_str("[integrations.cargo-workspace]\nworkspace-package = true\n");
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();
    ws
}

fn assert_parseable_and_marked(ws: &Path, after: &str) {
    let path = ws.join("projects/app/Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{after} must leave {}: {e}", path.display()));
    let doc: toml_edit::DocumentMut = text
        .parse()
        .unwrap_or_else(|e| panic!("{after} left a Cargo.toml that does not parse: {e}\n{text}"));
    assert!(
        doc["workspace"]["package"].is_table(),
        "{after} must author [workspace.package]:\n{text}"
    );
    assert!(
        text.contains("# managed by rwv\n[workspace.package]"),
        "{after} must put the marker on the line above the header:\n{text}"
    );
}

#[test]
fn an_authoring_verb_leaves_workspace_package_parseable_and_marked() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());

    let (ok, out) = rwv(
        &["role", "github/acme/beta", "fork", "--no-materialize"],
        &ws,
    );
    assert!(ok, "`rwv role` must succeed:\n{out}");
    assert_parseable_and_marked(&ws, "`rwv role`");
}

#[test]
fn doctor_fix_leaves_workspace_package_parseable_and_marked() {
    require_cargo!();
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());

    let (_, out) = rwv(&["doctor", "--fix"], &ws);
    assert_parseable_and_marked(&ws, "`rwv doctor --fix`");
    assert!(
        out.contains("[fixed] core: regenerated integration content"),
        "`doctor --fix` must report the regeneration it ran:\n{out}"
    );

    let metadata = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(&ws)
        .output()
        .expect("cargo should run");
    assert!(
        metadata.status.success(),
        "cargo must accept the weave's Cargo.toml:\n{}",
        String::from_utf8_lossy(&metadata.stderr)
    );

    let (_, again) = rwv(&["doctor"], &ws);
    assert!(
        !again.contains("parsing") && !again.contains("verify failed"),
        "every later verb reads the file:\n{again}"
    );
}
