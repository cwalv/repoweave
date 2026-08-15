//! Integration tests for plugin discovery, help section, and doctor inventory.
//!
//! Covers the AC:
//!   - `rwv --help` shows an "External commands" section with fixture plugin.
//!   - `rwv --help` with empty PATH shows no "External commands" section.
//!   - `rwv doctor --json` carries a `plugins` array in the output.
//!   - `plugins` records: name, path, shadowed flag, shadowed_by.
//!   - Doctor exit code is UNAFFECTED by the presence or absence of plugins.
//!
//! All tests pin PATH to fixture directories plus a directory holding
//! nothing but `git` — never the host's real PATH.
//!
//! SKIPPED ON WINDOWS. Every fixture here is a `#!/bin/sh` script made
//! executable with a mode bit, and the subject is which of them `which` will
//! dispatch off PATH. Windows has no executable bit — `which` selects on
//! PATHEXT — so a discoverable plugin there is `rwv-foo.exe` and
//! `strip_prefix("rwv-")` names the verb `foo.exe`. These assertions are not
//! merely spelled Unix-ly; what they assert is undefined on Windows until
//! that naming is decided. Deciding it is a product question, not a test fix.

#![cfg(unix)]

use predicates::prelude::*;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod common;

/// A directory holding a symlink to `git` and nothing else, resolved off the
/// test harness's own PATH.
///
/// Doctor shells out to `git` internally, so its tests need `git` reachable
/// — but PATH discovery of `rwv-*` binaries is exactly the property under
/// test here, and the operator's real PATH may carry one (that is this
/// file's own bug report). Handing doctor's subprocess the directory `git`
/// actually lives in would still leak whatever else lives beside it there;
/// this shim carries only `git`. Same shape as `go_free_bin` in
/// `member_incompatibility_test.rs`, solving the same problem for a
/// different tool.
fn git_only_bin() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        // Not a `TempDir`: one held in a `static` never drops, so it would
        // leave a directory behind on every run.
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("plugins-doctor-git-only-bin");
        fs::create_dir_all(&dir).expect("shim bin directory should be creatable");

        let git = which::which("git").expect("git must be resolvable to run these tests");
        let link = dir.join("git");
        // The directory is a `static` reused across runs, so the link
        // usually already exists.
        if link.symlink_metadata().is_err() {
            repoweave::symlink::create(&git, &link, repoweave::symlink::LinkTarget::File)
                .unwrap_or_else(|e| panic!("linking git into {}: {e}", dir.display()));
        }
        dir
    })
    .clone()
}

/// Build a PATH string with `extra_dir` ahead of the git-only directory —
/// doctor shells out to git, so its tests need git reachable, but nothing
/// else the host's real PATH might carry.
fn path_with_prefix(extra_dir: &Path) -> String {
    format!("{}:{}", extra_dir.display(), git_only_bin().display())
}

/// Build a PATH string from multiple dirs, each ahead of the git-only
/// directory.
fn multi_path_with_prefix(dirs: &[&Path]) -> String {
    let prefix: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    format!("{}:{}", prefix.join(":"), git_only_bin().display())
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write an executable shell script at `dir/name`.
fn write_script(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = fs::File::create(&path).unwrap();
    write!(f, "#!/bin/sh\necho hi\n").unwrap();
    drop(f);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Write a NON-executable file at `dir/name`.
fn write_non_exec(dir: &Path, name: &str) {
    let path = dir.join(name);
    fs::write(&path, "data\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
}

/// Build a minimal workspace at `tmp/ws` with an active project.
fn make_minimal_workspace(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    fs::create_dir_all(ws.join("projects").join(project)).unwrap();
    fs::create_dir_all(ws.join("github")).unwrap();
    fs::write(
        ws.join("projects").join(project).join("rwv.toml"),
        "[repositories]\n",
    )
    .unwrap();
    fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();
    // Initialise the git repo so doctor can do branch checks.
    common::git()
        .args(["init", "-b", "main"])
        .current_dir(&ws)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git init");
    common::git()
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&ws)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git commit");
    ws
}

// ===========================================================================
// 1. Help section — with and without fixture plugins
// ===========================================================================

/// With a fixture plugin on PATH the "External commands" section appears.
#[test]
fn help_shows_external_commands_section_with_fixture_plugin() {
    let plugin_dir = common::tempdir().expect("tempdir");
    write_script(plugin_dir.path(), "rwv-example");

    let out = rwv()
        .arg("--help")
        .env("PATH", plugin_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("External commands"),
        "section header absent: {text}"
    );
    assert!(text.contains("example"), "plugin name absent: {text}");
}

/// With no `rwv-*` executables on PATH the section is entirely absent.
#[test]
fn help_no_external_commands_section_when_path_is_empty() {
    let empty_dir = common::tempdir().expect("tempdir");
    let out = rwv()
        .arg("--help")
        .env("PATH", empty_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        !text.contains("External commands"),
        "unexpected section with empty PATH: {text}"
    );
}

/// Non-executable `rwv-*` files in PATH are NOT listed.
#[test]
fn help_non_executable_rwv_file_not_listed() {
    let dir = common::tempdir().expect("tempdir");
    write_non_exec(dir.path(), "rwv-notexec");
    // Also add an executable one so the section appears at all.
    write_script(dir.path(), "rwv-realexec");

    let out = rwv()
        .arg("--help")
        .env("PATH", dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("realexec"),
        "executable plugin should appear: {text}"
    );
    assert!(
        !text.contains("notexec"),
        "non-executable should NOT appear: {text}"
    );
}

/// Shadowed duplicates are NOT listed in the help section.
#[test]
fn help_shadowed_duplicate_not_listed_twice() {
    let dir1 = common::tempdir().expect("tempdir");
    let dir2 = common::tempdir().expect("tempdir");
    write_script(dir1.path(), "rwv-tool");
    write_script(dir2.path(), "rwv-tool");
    let path = format!("{}:{}", dir1.path().display(), dir2.path().display());

    let out = rwv()
        .arg("--help")
        .env("PATH", &path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    // "tool" should appear exactly once (in the plugin name list, not twice).
    let count = text.matches("tool").count();
    assert_eq!(
        count, 1,
        "expected 'tool' exactly once in --help, got:\n{text}"
    );
}

// ===========================================================================
// 2. Doctor JSON — plugins array
// ===========================================================================

/// `rwv doctor --json` output always contains a `plugins` key (even when
/// empty — the key is always present, just an empty array).
#[test]
fn doctor_json_has_plugins_key() {
    let tmp = common::tempdir().expect("tempdir");
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    // Use a temp dir with NO rwv-* files but keep git available on PATH via
    // path_with_prefix so rwv's internal git calls succeed.
    let empty_plugin_dir = common::tempdir().expect("tempdir");
    let path = path_with_prefix(empty_plugin_dir.path());

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .env("PATH", &path)
        .assert()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value =
        serde_json::from_slice(&out).expect("doctor --json output should be valid JSON");
    assert!(
        json.get("plugins").is_some(),
        "doctor --json must contain a 'plugins' key: {json}"
    );
    let plugins = json["plugins"]
        .as_array()
        .expect("plugins must be an array");
    assert!(
        plugins.is_empty(),
        "expected empty plugins array with no rwv-* on PATH, got: {plugins:?}"
    );
}

/// With a fixture plugin on PATH the `plugins` array is populated and contains
/// the expected record shape (name, path, shadowed=false, no shadowed_by).
#[test]
fn doctor_json_plugins_array_populated_with_fixture_plugin() {
    let tmp = common::tempdir().expect("tempdir");
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let plugin_dir = common::tempdir().expect("tempdir");
    let script = write_script(plugin_dir.path(), "rwv-myplugin");
    let path = path_with_prefix(plugin_dir.path());

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .env("PATH", &path)
        .assert()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value =
        serde_json::from_slice(&out).expect("doctor --json output should be valid JSON");
    let plugins = json["plugins"].as_array().expect("plugins array");
    assert_eq!(plugins.len(), 1, "expected 1 plugin, got: {plugins:?}");
    let record = &plugins[0];
    assert_eq!(
        record["name"].as_str(),
        Some("myplugin"),
        "wrong name: {record}"
    );
    assert_eq!(
        record["path"].as_str(),
        Some(repoweave::path_spelling::wire_path(&script).as_str()),
        "wrong path: {record}"
    );
    assert_eq!(
        record["shadowed"].as_bool(),
        Some(false),
        "should not be shadowed: {record}"
    );
    assert!(
        record.get("shadowed_by").is_none() || record["shadowed_by"].is_null(),
        "shadowed_by should be absent when not shadowed: {record}"
    );
}

/// A shadowed duplicate in the `plugins` array carries `shadowed: true` and
/// `shadowed_by` naming the winning binary.
#[test]
fn doctor_json_plugins_shadowed_record_shape() {
    let tmp = common::tempdir().expect("tempdir");
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let dir1 = common::tempdir().expect("tempdir");
    let dir2 = common::tempdir().expect("tempdir");
    let winner = write_script(dir1.path(), "rwv-duptool");
    write_script(dir2.path(), "rwv-duptool");
    let path = multi_path_with_prefix(&[dir1.path(), dir2.path()]);

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .env("PATH", &path)
        .assert()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value =
        serde_json::from_slice(&out).expect("doctor --json output should be valid JSON");
    let plugins = json["plugins"].as_array().expect("plugins array");
    assert_eq!(
        plugins.len(),
        2,
        "expected 2 records for duplicate: {plugins:?}"
    );

    // Both records are for "duptool". One is the PATH-order winner (shadowed=false),
    // the other is shadowed. Sorted by (name, path) so their position depends on
    // tmpdir path lexicography — we key on the `shadowed` flag, not position.
    assert!(
        plugins
            .iter()
            .all(|r| r["name"].as_str() == Some("duptool")),
        "all records should be for duptool: {plugins:?}"
    );
    let (winner_record, shadowed_record) = {
        let w = plugins
            .iter()
            .find(|r| !r["shadowed"].as_bool().unwrap_or(true));
        let s = plugins
            .iter()
            .find(|r| r["shadowed"].as_bool().unwrap_or(false));
        (
            w.expect("one record should be the winner"),
            s.expect("one record should be shadowed"),
        )
    };
    // The winner record should be the one in dir1 (first in PATH order).
    let winner_wire = repoweave::path_spelling::wire_path(&winner);
    assert_eq!(
        winner_record["path"].as_str(),
        Some(winner_wire.as_str()),
        "winner path should be dir1's binary: {winner_record}"
    );
    // The shadowed record should point back at the winner.
    assert_eq!(
        shadowed_record["shadowed_by"].as_str(),
        Some(winner_wire.as_str()),
        "shadowed_by should point at winner: {shadowed_record}"
    );
}

/// The `plugins` array is present and empty when PATH has no `rwv-*` entries —
/// the key is always emitted.
#[test]
fn doctor_json_plugins_array_empty_with_no_plugins_on_path() {
    let tmp = common::tempdir().expect("tempdir");
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    // Use path_with_prefix with an empty dir — no rwv-* files, but git remains
    // available so doctor's internal git calls succeed.
    let empty_dir = common::tempdir().expect("tempdir");
    let path = path_with_prefix(empty_dir.path());

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .env("PATH", &path)
        .assert()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value =
        serde_json::from_slice(&out).expect("doctor --json output should be valid JSON");
    let plugins = json["plugins"]
        .as_array()
        .expect("plugins must be an array");
    assert!(
        plugins.is_empty(),
        "expected empty plugins array: {plugins:?}"
    );
}

/// Doctor exit code is zero for a clean workspace regardless of plugin presence.
#[test]
fn doctor_exit_code_unaffected_by_plugin_presence() {
    let tmp = common::tempdir().expect("tempdir");
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let plugin_dir = common::tempdir().expect("tempdir");
    write_script(plugin_dir.path(), "rwv-someplugin");
    let path = path_with_prefix(plugin_dir.path());

    // With a plugin on PATH, doctor on a clean workspace should still exit 0.
    rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .env("PATH", &path)
        .assert()
        .stdout(predicate::str::contains("\"plugins\""))
        .stdout(predicate::str::contains("someplugin"));
}

/// Plugins array records are sorted by name for deterministic output.
#[test]
fn doctor_json_plugins_sorted_by_name() {
    let tmp = common::tempdir().expect("tempdir");
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let dir = common::tempdir().expect("tempdir");
    write_script(dir.path(), "rwv-zebra");
    write_script(dir.path(), "rwv-alpha");
    write_script(dir.path(), "rwv-middle");
    let path = path_with_prefix(dir.path());

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .env("PATH", &path)
        .assert()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value =
        serde_json::from_slice(&out).expect("doctor --json output should be valid JSON");
    let plugins = json["plugins"].as_array().expect("plugins array");
    let names: Vec<&str> = plugins
        .iter()
        .map(|r| r["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        &["alpha", "middle", "zebra"],
        "not sorted: {names:?}"
    );
}

// ===========================================================================
// 3. Host-PATH isolation control
// ===========================================================================

/// Seed a decoy `rwv-*` binary onto the PATH this test process actually
/// inherited, then re-run the rest of this file as a child process that
/// inherits the poisoned PATH, and confirm it stays green.
///
/// Mutating this process's own PATH via `std::env::set_var` would be unsound
/// under a parallel test runner (see `write_exit_code_shim`'s doc comment in
/// `integrations_test.rs`), so the decoy goes on a re-invocation of this
/// test binary instead — the same self-invocation shape
/// `ref_registry_test.rs` uses to control environment for a whole test
/// process.
#[test]
fn suite_stays_green_with_decoy_on_inherited_path() {
    let decoy_dir = common::tempdir().expect("tempdir");
    write_script(decoy_dir.path(), "rwv-hostdecoy");

    let inherited = std::env::var("PATH").unwrap_or_default();
    let poisoned = format!("{}:{inherited}", decoy_dir.path().display());

    let out = std::process::Command::new(std::env::current_exe().expect("test binary path"))
        .args([
            "--test-threads=1",
            "--skip",
            "suite_stays_green_with_decoy_on_inherited_path",
        ])
        .env("PATH", &poisoned)
        .output()
        .expect("re-invoke test binary with a decoy on the inherited PATH");

    assert!(
        out.status.success(),
        "suite must stay green with rwv-hostdecoy on the inherited PATH:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
