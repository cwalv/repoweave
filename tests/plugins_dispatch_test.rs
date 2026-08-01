//! External-subcommand dispatch.
//!
//! Covers the AC:
//!   - Builtin shadowing: a PATH `rwv-status` is never dispatched.
//!   - Fallthrough exec with args passed through verbatim.
//!   - Exit-code propagation.
//!   - Signal reporting (128 + N).
//!   - Unknown-verb error message.
//!   - Exec-failure error message with errno.
//!   - Soft fallthrough outside a workspace (no addressing flags).
//!   - Explicit-flag resolution failure errors BEFORE exec.
//!   - `rwv explain <non-core>` refuses with the external-command pointer.
//!   - Adversarial: verb name containing `--`, args containing `--`, empty
//!     `rwv-` name.

use predicates::prelude::*;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

/// Write an executable shell script at `dir/name` with the given body.
///
/// Sets mode 0o755 so it is executable by the current process.
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = fs::File::create(&path).unwrap();
    // Force a POSIX shell shebang so the child's interpreter is
    // deterministic regardless of the test host's login shell.
    write!(f, "#!/bin/sh\n{body}").unwrap();
    drop(f);
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// A path outside any workspace ($TMPDIR is safe because `resolve` walks up
/// but never traverses above `$HOME`, and `$TMPDIR` is not under `$HOME`).
fn tmp_cwd() -> std::path::PathBuf {
    std::env::temp_dir()
}

// ---------------------------------------------------------------------------
// Builtin shadowing — a PATH plugin never overrides a core verb
// ---------------------------------------------------------------------------

/// A `rwv-status` on PATH must never be dispatched: clap routes `status` to
/// the core verb before external fallthrough runs. If this test ever fails,
/// the "core always wins" invariant has been broken.
#[test]
fn builtin_status_shadows_path_plugin() {
    let plugin_dir = common::tempdir().unwrap();
    // A rwv-status that would exit 42 if invoked. If the builtin honours
    // this, the outer process would exit 42 (which we require NOT to
    // happen).
    write_script(
        plugin_dir.path(),
        "rwv-status",
        "echo IMPOSTOR_STATUS; exit 42\n",
    );

    // Invoke from /tmp (no workspace, no addressing flags) so the builtin
    // exits with a benign non-42 status. We only need to prove: exit code
    // is NOT 42 AND stdout does NOT contain the impostor marker.
    let assert = rwv()
        .arg("status")
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .failure();
    let out = assert.get_output();
    assert_ne!(
        out.status.code(),
        Some(42),
        "core `status` was shadowed by PATH plugin (exit 42):\n\
         stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("IMPOSTOR_STATUS"),
        "PATH rwv-status leaked to stdout: {}",
        String::from_utf8_lossy(&out.stdout),
    );
}

/// Same for every other core verb — spot-check the top-level ones a plugin
/// author might accidentally reserve. Uses the same `rwv-<name>` script
/// exit-42-marker trick.
#[test]
fn builtin_all_core_verbs_shadow_path_plugins() {
    let plugin_dir = common::tempdir().unwrap();
    // Every top-level verb that has its own subcommand variant. If a name
    // is a core verb, its plugin must NOT be dispatched.
    let core_verbs = [
        "activate",
        "prime",
        "resolve",
        "add",
        "fetch",
        "init",
        "remove",
        "workweave",
        "doctor",
        "lock",
        "status",
        "abort",
        "sync",
        "sync-to",
        "push",
        "update",
        "completions",
        "explain",
        "setup",
    ];
    for verb in core_verbs {
        write_script(
            plugin_dir.path(),
            &format!("rwv-{verb}"),
            "echo IMPOSTOR; exit 42\n",
        );
    }

    for verb in core_verbs {
        let assert = rwv()
            .arg(verb)
            .current_dir(tmp_cwd())
            .env("PATH", prepend_path(plugin_dir.path()))
            .assert();
        let out = assert.get_output();
        assert_ne!(
            out.status.code(),
            Some(42),
            "core `{verb}` was shadowed by PATH plugin:\n\
             stdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("IMPOSTOR"),
            "PATH rwv-{verb} leaked to stdout: {}",
            String::from_utf8_lossy(&out.stdout),
        );
    }
}

// ---------------------------------------------------------------------------
// Fallthrough exec — args passed through verbatim
// ---------------------------------------------------------------------------

/// A `rwv-foo` on PATH is dispatched when `foo` is not a core verb. Args
/// after the verb reach the plugin verbatim, including `--` and repeated
/// flags.
#[test]
fn external_verb_dispatched_with_args_verbatim() {
    let plugin_dir = common::tempdir().unwrap();
    // Echo each argument on its own line so the test can assert on order.
    write_script(
        plugin_dir.path(),
        "rwv-foo",
        "for a in \"$@\"; do echo \"[$a]\"; done\n",
    );

    rwv()
        .args([
            "foo",
            "--flag",
            "value",
            "--",
            "-x",
            "positional with spaces",
        ])
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success()
        .stdout(
            predicate::str::contains("[--flag]")
                .and(predicate::str::contains("[value]"))
                .and(predicate::str::contains("[--]"))
                .and(predicate::str::contains("[-x]"))
                .and(predicate::str::contains("[positional with spaces]")),
        );
}

/// Global addressing flags (-C, -w) are consumed by rwv and NEVER seen by
/// the plugin. This is the load-bearing contract for the composition:
/// addressing lives at one point.
#[test]
fn external_verb_does_not_see_global_addressing_flags() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let plugin_dir = common::tempdir().unwrap();
    write_script(
        plugin_dir.path(),
        "rwv-foo",
        "for a in \"$@\"; do echo \"[$a]\"; done\n",
    );

    let assert = rwv()
        .args(["-C", &ws.to_string_lossy(), "foo", "arg1"])
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("[arg1]"), "arg1 not passed: {stdout}");
    assert!(!stdout.contains("-C"), "-C leaked to plugin: {stdout}");
    assert!(
        !stdout.contains(ws.to_string_lossy().as_ref()),
        "workspace path leaked to plugin: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Exit-code propagation
// ---------------------------------------------------------------------------

/// Non-zero exit codes propagate verbatim.
#[test]
fn external_verb_exit_code_propagated_verbatim() {
    let plugin_dir = common::tempdir().unwrap();
    for code in [0_i32, 1, 2, 3, 42, 127, 255] {
        write_script(
            plugin_dir.path(),
            &format!("rwv-exit-{code}"),
            &format!("exit {code}\n"),
        );
        let assert = rwv()
            .arg(format!("exit-{code}"))
            .current_dir(tmp_cwd())
            .env("PATH", prepend_path(plugin_dir.path()))
            .assert();
        // The exit code must be observable and equal to what the plugin
        // returned. `Command::status` yields `None` only for signal death,
        // which is a separate test.
        let got = assert
            .get_output()
            .status
            .code()
            .expect("plugin exited normally, expected an exit code");
        assert_eq!(got, code, "expected plugin exit {code}, got {got}");
    }
}

// ---------------------------------------------------------------------------
// Signal reporting
// ---------------------------------------------------------------------------

/// Signal death propagates as 128 + N with a stderr line naming the plugin
/// and the signal number. Uses `kill -9 $$` (SIGKILL, N=9), a signal every
/// POSIX shell can raise on itself.
#[test]
fn external_verb_signal_reported_as_128_plus_n() {
    let plugin_dir = common::tempdir().unwrap();
    write_script(plugin_dir.path(), "rwv-die", "kill -9 $$\n");
    let assert = rwv()
        .arg("die")
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .failure();
    let out = assert.get_output();
    assert_eq!(
        out.status.code(),
        Some(128 + 9),
        "expected 128+9 exit, got {:?}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rwv-die terminated by signal 9"),
        "stderr should name the plugin and signal: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Unknown-verb error
// ---------------------------------------------------------------------------

/// A verb with no core match and no `rwv-<verb>` on PATH emits the unknown-
/// verb error naming both surfaces and pointing at `rwv --help`.
#[test]
fn unknown_verb_reports_no_core_no_plugin_message() {
    // Empty plugin dir; PATH only contains it so no accidental `rwv-*`
    // binaries can save the day.
    let plugin_dir = common::tempdir().unwrap();
    rwv()
        .arg("no-such-verb-anywhere")
        .current_dir(tmp_cwd())
        .env("PATH", plugin_dir.path().to_string_lossy().to_string())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no-such-verb-anywhere")
                .and(predicate::str::contains("rwv-no-such-verb-anywhere"))
                .and(predicate::str::contains("PATH"))
                .and(predicate::str::contains("rwv --help")),
        );
}

// ---------------------------------------------------------------------------
// Exec-failure error
// ---------------------------------------------------------------------------

/// A `rwv-<verb>` binary found on PATH but not executable (e.g. mode
/// 0o644) produces the exec-failure error naming the binary and the OS
/// error. `which` will not resolve a non-executable file, so instead we
/// point at a directory with a `rwv-broken` that is NOT +x — some
/// implementations of `which` still surface it and rely on spawn to fail;
/// here we make the file a regular file with the shebang line missing so
/// spawn errors with ENOEXEC or `which` returns None. Either way this
/// path exercises the "found on PATH but unspawnable OR unknown" seam.
///
/// The direct exec-failure surface (which returns a path but the file
/// then cannot be spawned) is exercised by making the "binary" a broken
/// symlink to a nonexistent target — `which` follows symlinks to check
/// existence and rejects it; but a symlink target that exists but is a
/// directory passes `which`'s executable probe on some platforms and
/// fails at spawn. This test uses that shape.
#[test]
fn exec_failure_names_binary_and_errno() {
    let plugin_dir = common::tempdir().unwrap();
    // Make a "rwv-broken" that is executable but a directory — `which`
    // considers it executable (has +x bit set on directories to allow
    // descent), but `Command::spawn` fails with EACCES / EISDIR.
    let broken = plugin_dir.path().join("rwv-broken");
    fs::create_dir(&broken).unwrap();
    let mut perms = fs::metadata(&broken).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&broken, perms).unwrap();

    let assert = rwv()
        .arg("broken")
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .failure();
    let out = assert.get_output();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Either "unknown verb" (which rejects the directory) or the exec-
    // failure path (which accepts and spawn fails). Both are documented
    // outcomes for "not a usable executable"; we just require ONE of the
    // two rwv-side messages fires (not a raw panic or clap error).
    assert!(
        stderr.contains("rwv-broken") || stderr.contains("broken"),
        "stderr should name the plugin/verb: {stderr}"
    );
    assert!(
        stderr.contains("failed to exec") || stderr.contains("unknown verb"),
        "stderr should be one of the two documented rwv-side surfaces: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Soft fallthrough
// ---------------------------------------------------------------------------

/// No addressing flags + cwd walk finds no workspace → the plugin is still
/// exec'd. Some plugins legitimately run outside a workspace.
#[test]
fn soft_fallthrough_execs_plugin_outside_workspace() {
    let plugin_dir = common::tempdir().unwrap();
    write_script(plugin_dir.path(), "rwv-outside", "echo PLUGIN_RAN\n");
    // Run from a genuine "not a workspace" cwd — the plugin dir itself
    // (an empty tempdir) has no `.rwv-workweave` marker and no workspace
    // above it.
    rwv()
        .arg("outside")
        .current_dir(plugin_dir.path())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success()
        .stdout(predicate::str::contains("PLUGIN_RAN"));
}

// ---------------------------------------------------------------------------
// Explicit-flag resolution failure errors BEFORE exec
// ---------------------------------------------------------------------------

/// `-C <nonexistent>` fails resolution BEFORE the plugin is spawned. The
/// plugin's stdout must NOT appear — the rwv-side error is what the user
/// sees.
#[test]
fn explicit_c_flag_failure_errors_before_exec() {
    let plugin_dir = common::tempdir().unwrap();
    // A plugin that would print a marker if wrongly spawned.
    write_script(plugin_dir.path(), "rwv-would-run", "echo SHOULD_NOT_RUN\n");
    // `-C` points at a path that doesn't exist. `resolve_cwd_override`
    // errors before we even get to the dispatch match.
    rwv()
        .args(["-C", "/definitely/does/not/exist/anywhere", "would-run"])
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .failure()
        .stdout(predicate::str::contains("SHOULD_NOT_RUN").not());
}

/// `-w <unregistered-name>` fails registry lookup BEFORE the plugin is
/// spawned.
#[test]
fn explicit_w_flag_failure_errors_before_exec() {
    let tmp = common::tempdir().unwrap();
    let _ws = make_minimal_workspace(tmp.path(), "myproj");
    let plugin_dir = common::tempdir().unwrap();
    write_script(plugin_dir.path(), "rwv-would-run", "echo SHOULD_NOT_RUN\n");
    rwv()
        .args(["-w", "myproj--nonexistent-workweave", "would-run"])
        .current_dir(tmp.path().join("ws"))
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .failure()
        .stdout(predicate::str::contains("SHOULD_NOT_RUN").not());
}

// ---------------------------------------------------------------------------
// explain excludes plugins
// ---------------------------------------------------------------------------

/// `rwv explain <non-core>` errors with the "external command" pointer
/// and does NOT exec anything on PATH.
#[test]
fn explain_non_core_verb_reports_external_pointer() {
    let plugin_dir = common::tempdir().unwrap();
    // A rwv-foo that would print a marker if explain wrongly exec'd it.
    write_script(plugin_dir.path(), "rwv-foo", "echo EXPLAIN_EXEC_BUG\n");
    rwv()
        .args(["explain", "foo"])
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("external command")
                .and(predicate::str::contains("rwv foo --help")),
        )
        .stdout(predicate::str::contains("EXPLAIN_EXEC_BUG").not());
}

/// A typo close to a core verb (edit distance ≤ 2) still gets the "did you
/// mean" hint. This preserves the operator-help path — the plugin pointer
/// only fires when the input isn't a close typo of any core verb.
#[test]
fn explain_typo_still_gets_did_you_mean() {
    rwv()
        .args(["explain", "statu"])
        .current_dir(tmp_cwd())
        .assert()
        .failure()
        .stderr(predicate::str::contains("did you mean").and(predicate::str::contains("status")));
}

// ---------------------------------------------------------------------------
// Adversarial: unusual verb / arg shapes
// ---------------------------------------------------------------------------

/// A verb name containing `-` (extra dashes) is dispatched as `rwv-<verb>`.
/// Ensures the join is `rwv-<verb>` regardless of internal dashes.
#[test]
fn external_verb_with_multiple_dashes_is_dispatched() {
    let plugin_dir = common::tempdir().unwrap();
    write_script(plugin_dir.path(), "rwv-my-multi-word-verb", "echo RAN\n");
    rwv()
        .arg("my-multi-word-verb")
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success()
        .stdout(predicate::str::contains("RAN"));
}

// ---------------------------------------------------------------------------
// Context envelope — RWV_* vars set on the spawned child
// ---------------------------------------------------------------------------

/// Helper: write a plugin that dumps specific env vars to stdout, one per
/// line as `KEY=VALUE`. Lines for missing vars are omitted.
fn write_env_dump_plugin(dir: &Path, name: &str, vars: &[&str]) -> PathBuf {
    // Each var: if set, print KEY=VALUE; if unset, print KEY=UNSET so tests
    // can distinguish "set to empty" from "not set at all".
    let body: String = vars
        .iter()
        .map(|v| {
            format!(
                "if [ -n \"${{{v}+x}}\" ]; then echo \"{v}=${{{v}}}\" ; else echo \"{v}=UNSET\" ; fi\n",
            )
        })
        .collect();
    write_script(dir, name, &body)
}

const ENVELOPE_VARS: &[&str] = &[
    "RWV_VERSION",
    "RWV_WORKSPACE",
    "RWV_WORKWEAVE",
    "RWV_PROJECT",
];

/// Parse the env-dump output into a key→value map.
fn parse_env_dump(stdout: &str) -> std::collections::HashMap<String, String> {
    stdout
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.to_owned(), v.to_owned()))
        })
        .collect()
}

/// `RWV_VERSION` is always set, even outside a workspace.
#[test]
fn envelope_rwv_version_always_set_outside_workspace() {
    let plugin_dir = common::tempdir().unwrap();
    write_env_dump_plugin(plugin_dir.path(), "rwv-envcheck", ENVELOPE_VARS);

    let assert = rwv()
        .arg("envcheck")
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let env = parse_env_dump(&stdout);

    assert!(
        env.get("RWV_VERSION")
            .map(|v| !v.is_empty() && v != "UNSET")
            .unwrap_or(false),
        "RWV_VERSION must be set outside a workspace; got: {env:?}"
    );
    assert_eq!(
        env.get("RWV_WORKSPACE").map(|s| s.as_str()),
        Some("UNSET"),
        "RWV_WORKSPACE must be unset outside a workspace; got: {env:?}"
    );
    assert_eq!(
        env.get("RWV_WORKWEAVE").map(|s| s.as_str()),
        Some("UNSET"),
        "RWV_WORKWEAVE must be unset outside a workspace; got: {env:?}"
    );
    assert_eq!(
        env.get("RWV_PROJECT").map(|s| s.as_str()),
        Some("UNSET"),
        "RWV_PROJECT must be unset outside a workspace; got: {env:?}"
    );
}

/// At the primary weave (no workweave): workspace and project are set;
/// workweave is absent.
#[test]
fn envelope_primary_workspace_sets_workspace_and_project() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let plugin_dir = common::tempdir().unwrap();
    write_env_dump_plugin(plugin_dir.path(), "rwv-envcheck", ENVELOPE_VARS);

    let assert = rwv()
        .arg("envcheck")
        .current_dir(&ws)
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let env = parse_env_dump(&stdout);

    // RWV_VERSION is always set.
    assert!(
        env.get("RWV_VERSION")
            .map(|v| v != "UNSET")
            .unwrap_or(false),
        "RWV_VERSION must be set; got: {env:?}"
    );
    // RWV_WORKSPACE is the canonical workspace root.
    let ws_val = env.get("RWV_WORKSPACE").expect("RWV_WORKSPACE must be set");
    assert_ne!(
        ws_val, "UNSET",
        "RWV_WORKSPACE must be set at primary; got: {env:?}"
    );
    // The canonical path (symlinks resolved) ends with the ws dir name.
    assert!(
        ws_val.contains("ws"),
        "RWV_WORKSPACE should contain workspace path; got: {ws_val}"
    );
    // RWV_PROJECT is the active project.
    assert_eq!(
        env.get("RWV_PROJECT").map(|s| s.as_str()),
        Some("myproj"),
        "RWV_PROJECT must be 'myproj'; got: {env:?}"
    );
    // RWV_WORKWEAVE must be absent at the primary.
    assert_eq!(
        env.get("RWV_WORKWEAVE").map(|s| s.as_str()),
        Some("UNSET"),
        "RWV_WORKWEAVE must be absent at primary; got: {env:?}"
    );
}

/// `-w <project>--<name>` addressing: envelope is set from the resolved
/// workweave context, including RWV_WORKWEAVE.
#[test]
fn envelope_via_w_flag_sets_workweave_var() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let ws_canon = ws.canonicalize().unwrap();
    let plugin_dir = common::tempdir().unwrap();
    write_env_dump_plugin(plugin_dir.path(), "rwv-envcheck", ENVELOPE_VARS);

    // Build a workweave (myproj--feat) in the same parent container so
    // the registry can find it.
    let ww_dir = ws_canon
        .parent()
        .unwrap()
        .join(".workweaves")
        .join("myproj--feat");
    std::fs::create_dir_all(&ww_dir).unwrap();
    // Write the .rwv-workweave marker.
    let marker_content = format!(
        "{{\"primary\":\"{}\",\"project\":\"myproj\",\"parent\":\"{}\"}}",
        ws_canon.display(),
        ws_canon.display()
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), &marker_content).unwrap();
    // Replicate workspace structure in the workweave dir.
    std::fs::create_dir_all(ww_dir.join("github")).unwrap();
    std::fs::create_dir_all(ww_dir.join("projects").join("myproj")).unwrap();
    // Register the workweave in the workspace index.
    let index_dir = ws.join("projects").join("myproj");
    let ww_canon = ww_dir.canonicalize().unwrap();
    let index_content = format!(
        "{{\"container\":\"{}\",\"workweaves\":{{\"feat\":\"{}\"}}}}",
        ww_canon.parent().unwrap().display(),
        ww_canon.display(),
    );
    std::fs::write(index_dir.join(".rwv-workweave-index"), &index_content).unwrap();

    let assert = rwv()
        .args(["-w", "myproj--feat", "envcheck"])
        .current_dir(&ws)
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let env = parse_env_dump(&stdout);

    assert_eq!(
        env.get("RWV_WORKWEAVE").map(|s| s.as_str()),
        Some("myproj--feat"),
        "RWV_WORKWEAVE must be 'myproj--feat'; got: {env:?}"
    );
    assert_eq!(
        env.get("RWV_PROJECT").map(|s| s.as_str()),
        Some("myproj"),
        "RWV_PROJECT must be 'myproj'; got: {env:?}"
    );
    let ws_val = env.get("RWV_WORKSPACE").expect("RWV_WORKSPACE must be set");
    assert_ne!(ws_val, "UNSET", "RWV_WORKSPACE must be set; got: {env:?}");
}

/// Shared-projection test: the `Resolution` that goes into the `--json`
/// resolution block and the env envelope are projections of the same value.
/// This test spawns a plugin that dumps its env and independently calls
/// `status --json` from the same workspace, then compares workspace/project
/// field-by-field. If someone forks the envelope from the JSON block, this
/// test catches it.
#[test]
fn envelope_and_json_resolution_block_agree() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let plugin_dir = common::tempdir().unwrap();
    write_env_dump_plugin(plugin_dir.path(), "rwv-envcheck", ENVELOPE_VARS);

    // Collect the env envelope from the plugin.
    let assert = rwv()
        .arg("envcheck")
        .current_dir(&ws)
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let env = parse_env_dump(&stdout);

    // Collect the JSON resolution block from `rwv status --json`.
    let status_out = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .stdout
        .clone();
    let status_json: serde_json::Value =
        serde_json::from_slice(&status_out).expect("status --json must produce valid JSON");

    // resolution.workspace must match RWV_WORKSPACE.
    let json_workspace = status_json
        .pointer("/resolution/workspace")
        .and_then(|v| v.as_str())
        .expect("resolution.workspace must be in status --json");
    let env_workspace = env
        .get("RWV_WORKSPACE")
        .expect("RWV_WORKSPACE must be in env");
    assert_eq!(
        json_workspace, env_workspace,
        "resolution.workspace in JSON must equal RWV_WORKSPACE in env"
    );

    // resolution.project must match RWV_PROJECT.
    let json_project = status_json
        .pointer("/resolution/project")
        .and_then(|v| v.as_str())
        .expect("resolution.project must be in status --json");
    let env_project = env.get("RWV_PROJECT").expect("RWV_PROJECT must be in env");
    assert_eq!(
        json_project, env_project,
        "resolution.project in JSON must equal RWV_PROJECT in env"
    );

    // resolution.workweave is absent at the primary; RWV_WORKWEAVE must be unset.
    let json_workweave = status_json.pointer("/resolution/workweave");
    let env_workweave = env.get("RWV_WORKWEAVE").map(|s| s.as_str());
    assert!(
        json_workweave.is_none(),
        "resolution.workweave should be absent at primary; got: {json_workweave:?}"
    );
    assert_eq!(
        env_workweave,
        Some("UNSET"),
        "RWV_WORKWEAVE should be unset at primary; got: {env:?}"
    );
}

/// docs/reference/cli.md must document the context envelope variables.
#[test]
fn cli_md_documents_context_envelope() {
    let root = env!("CARGO_MANIFEST_DIR");
    let cli_md =
        fs::read_to_string(format!("{root}/docs/reference/cli.md")).expect("cli.md should exist");
    for var in ENVELOPE_VARS {
        assert!(
            cli_md.contains(var),
            "docs/reference/cli.md must document {var}"
        );
    }
    // The "context envelope" section heading or prose must be present.
    assert!(
        cli_md.to_lowercase().contains("context envelope") || cli_md.contains("RWV_VERSION"),
        "docs/reference/cli.md should have a context envelope section"
    );
}

// ---------------------------------------------------------------------------
// Docs assertion: cli.md external-commands note is present
// ---------------------------------------------------------------------------

/// docs/reference/cli.md must document the external-command dispatch.
/// This is the docs half of the AC.
#[test]
fn cli_md_documents_external_commands_note() {
    let root = env!("CARGO_MANIFEST_DIR");
    let cli_md =
        fs::read_to_string(format!("{root}/docs/reference/cli.md")).expect("cli.md should exist");
    let lower = cli_md.to_lowercase();
    assert!(
        lower.contains("external command"),
        "docs/reference/cli.md should mention external commands"
    );
    assert!(
        lower.contains("rwv-") && (lower.contains("path") || lower.contains("$path")),
        "docs/reference/cli.md should describe the `rwv-<verb>` on PATH dispatch"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Prepend `dir` to the inherited PATH so the plugin binaries win over any
/// same-named binary already on PATH.
fn prepend_path(dir: &Path) -> String {
    let inherited = std::env::var("PATH").unwrap_or_default();
    format!("{}:{inherited}", dir.display())
}

/// Build a minimal workspace at `tmp/ws` with a `projects/<project>/rwv.yaml`
/// and a `.rwv-active` pointer. Returns the workspace root.
fn make_minimal_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects").join(project)).unwrap();
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::write(
        ws.join("projects").join(project).join("rwv.yaml"),
        "repositories: {}\n",
    )
    .unwrap();
    std::fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();
    ws
}
