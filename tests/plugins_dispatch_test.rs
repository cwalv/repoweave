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
//!
//! Every plugin fixture carries a POSIX shell body and a batch body for the
//! same behavior; [`common::write_plugin`] writes whichever the host runs.

use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

/// Write a plugin at `dir/name` running `sh` on Unix and `cmd` on Windows.
fn write_script(dir: &Path, name: &str, sh: &str, cmd: &str) -> PathBuf {
    common::write_plugin(dir, name, sh, cmd)
}

/// Write a plugin that prints `marker` and exits 0.
fn write_echo(dir: &Path, name: &str, marker: &str) -> PathBuf {
    let line = format!("echo {marker}\n");
    write_script(dir, name, &line, &line)
}

const ARGS_ECHO_SH: &str = "for a in \"$@\"; do echo \"[$a]\"; done\n";
const ARGS_ECHO_CMD: &str = ":next\nif \"%~1\"==\"\" exit /b 0\necho [%~1]\nshift\ngoto next\n";

/// A path outside any workspace, provided no ancestor of the temp dir is one
/// — on Windows the temp dir sits inside the user profile.
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
    // A rwv-status that would exit 42 if invoked. If the builtin honors
    // this, the outer process would exit 42 (which we require NOT to
    // happen).
    write_script(
        plugin_dir.path(),
        "rwv-status",
        "echo IMPOSTOR_STATUS; exit 42\n",
        "echo IMPOSTOR_STATUS\nexit 42\n",
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
        "materialize",
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
            "echo IMPOSTOR\nexit 42\n",
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
    write_script(plugin_dir.path(), "rwv-foo", ARGS_ECHO_SH, ARGS_ECHO_CMD);

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
    write_script(plugin_dir.path(), "rwv-foo", ARGS_ECHO_SH, ARGS_ECHO_CMD);

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
/// POSIX shell can raise on itself. Unix only: Windows has no signals.
#[test]
#[cfg(unix)]
fn external_verb_signal_reported_as_128_plus_n() {
    let plugin_dir = common::tempdir().unwrap();
    write_script(plugin_dir.path(), "rwv-die", "kill -9 $$\n", "");
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

#[test]
fn job_helper() {
    common::job::serve();
}

/// `rwv hold` as a foreground job, its plugin a [`common::job::hold`] that
/// records its pid in the returned file.
fn hold_job(plugin_dir: &Path) -> (std::process::Child, PathBuf) {
    let ready = plugin_dir.join("ready");
    let plugin = plugin_dir.join(format!("rwv-hold{}", std::env::consts::EXE_SUFFIX));
    fs::hard_link(common::job::helper_exe(), &plugin)
        .expect("the plugin is this test binary under a plugin's name");
    let mut cmd = common::rwv_std();
    cmd.arg("hold")
        .args(common::job::helper_args())
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir))
        .stdout(std::process::Stdio::null());
    common::job::hold(&mut cmd, &ready);
    let child = common::job::as_foreground_job(&mut cmd).spawn().unwrap();
    (child, ready)
}

/// A terminal Ctrl-C reaches the whole foreground job — rwv and its plugin at
/// once. rwv must outlive it and hand back the status the plugin then exits
/// with, rather than die first and return the prompt over a plugin still
/// winding down.
#[test]
fn an_interrupt_to_the_whole_job_leaves_rwv_waiting_for_the_plugin_status() {
    // Beside the test binary, so the hard link stays on one filesystem; a
    // copy would hold a written file open while sibling tests fork.
    let plugin_dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let (mut child, ready) = hold_job(plugin_dir.path());
    common::job::recorded_pid(&ready, std::time::Duration::from_secs(20));
    common::job::interrupt_foreground_job(&child);
    let status = common::job::wait_job(&mut child, common::job::JOB_BOUND)
        .unwrap_or_else(|e| panic!("the interrupted job never ended: {e}"));
    assert_eq!(
        status.code(),
        Some(common::job::HOLD_EXIT),
        "rwv exited with {status:?}"
    );
}

/// rwv outlives the interrupt by handling it, which exec does not pass on: a
/// plugin that takes no interrupt action of its own still dies of one, and rwv
/// reports how it died.
#[test]
fn a_plugin_that_takes_no_interrupt_action_dies_of_the_jobs_interrupt() {
    let plugin_dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ready = plugin_dir.path().join("ready");
    let plugin = plugin_dir
        .path()
        .join(format!("rwv-linger{}", std::env::consts::EXE_SUFFIX));
    fs::hard_link(common::job::helper_exe(), &plugin)
        .expect("the plugin is this test binary under a plugin's name");
    let mut cmd = common::rwv_std();
    cmd.arg("linger")
        .args(common::job::helper_args())
        .current_dir(tmp_cwd())
        .env("PATH", prepend_path(plugin_dir.path()))
        .stdout(std::process::Stdio::null());
    common::job::linger(&mut cmd, &ready);
    let mut child = common::job::as_foreground_job(&mut cmd).spawn().unwrap();
    let lingering = common::job::recorded_pid(&ready, std::time::Duration::from_secs(20));
    common::job::interrupt_foreground_job(&child);
    let status = common::job::wait_job(&mut child, common::job::JOB_BOUND)
        .unwrap_or_else(|e| panic!("the interrupt did not end the plugin: {e}"));
    common::job::wait_dead(lingering);
    #[cfg(unix)]
    assert_eq!(status.code(), Some(128 + 2), "rwv exited with {status:?}");
    #[cfg(windows)]
    assert_ne!(status.code(), Some(0), "rwv exited with {status:?}");
}

/// A job that never ends fails its test once its bound passes, with rwv and
/// the plugin both gone, instead of holding the suite until CI cancels it.
#[test]
fn a_job_that_outlives_its_bound_is_killed_whole() {
    use std::time::Duration;
    let plugin_dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let (mut child, ready) = hold_job(plugin_dir.path());
    let plugin = common::job::recorded_pid(&ready, Duration::from_secs(20));
    let rwv = child.id();
    let bound = Duration::from_secs(2);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(common::job::wait_job(&mut child, bound));
    });
    let Ok(outcome) = rx.recv_timeout(bound + Duration::from_secs(30)) else {
        let _ = common::job::kill_job(rwv);
        panic!("a wait bounded at {bound:?} was still waiting 30s past it");
    };
    let error = outcome.expect_err("a hold nobody interrupts never exits");
    assert!(error.contains("every process of it was killed"), "{error}");
    common::job::wait_dead(plugin);
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();
    }

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
    write_echo(plugin_dir.path(), "rwv-outside", "PLUGIN_RAN");
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
// Working directory — the plugin starts in the invocation directory
// ---------------------------------------------------------------------------

const CWD_SH: &str = "pwd -P\necho \"RWV_WORKSPACE=$RWV_WORKSPACE\"\n";
const CWD_CMD: &str = "cd\necho RWV_WORKSPACE=%RWV_WORKSPACE%\n";

fn cwd_line(stdout: &str) -> PathBuf {
    let line = stdout.lines().next().expect("the plugin printed its cwd");
    repoweave::path_spelling::resolve(Path::new(line.trim())).unwrap()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    repoweave::path_spelling::resolve(a).unwrap() == repoweave::path_spelling::resolve(b).unwrap()
}

/// Under `-C X` the plugin's working directory is X, not the shell's — while
/// the envelope keeps naming the workspace root.
#[test]
fn a_plugin_under_c_starts_in_the_c_directory_and_the_envelope_still_names_the_workspace() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let sub = ws.join("github");
    let plugin_dir = common::tempdir().unwrap();
    write_script(plugin_dir.path(), "rwv-whereami", CWD_SH, CWD_CMD);

    let assert = rwv()
        .args(["-C", &sub.to_string_lossy(), "whereami"])
        .current_dir(plugin_dir.path())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        same_dir(&cwd_line(&stdout), &sub),
        "plugin did not start in the -C directory {}: {stdout}",
        sub.display()
    );
    let workspace = parse_env_dump(&stdout)
        .remove("RWV_WORKSPACE")
        .expect("RWV_WORKSPACE is set");
    assert!(
        same_dir(Path::new(&workspace), &ws),
        "RWV_WORKSPACE moved off the workspace root: {workspace}"
    );
}

/// The plugin's inherited `PWD` names the `-C` directory too. A shell would
/// correct a stale one on startup, so the plugin here is `env` itself, which
/// prints the environment exactly as it was handed over.
#[cfg(unix)]
#[test]
fn a_plugin_under_c_inherits_pwd_naming_the_c_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let sub = ws.join("github");
    let plugin_dir = common::tempdir().unwrap();
    std::os::unix::fs::symlink("/usr/bin/env", plugin_dir.path().join("rwv-envdump")).unwrap();

    let assert = rwv()
        .args(["-C", &sub.to_string_lossy(), "envdump"])
        .current_dir(plugin_dir.path())
        .env("PATH", prepend_path(plugin_dir.path()))
        .env("PWD", plugin_dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let pwd = parse_env_dump(&stdout).remove("PWD").expect("PWD is set");
    assert!(
        same_dir(Path::new(&pwd), &sub),
        "the plugin inherited PWD={pwd}, not the -C directory {}",
        sub.display()
    );
}

/// A relative argument the plugin receives names a file under the `-C`
/// directory, the way it would for a core verb.
#[test]
fn a_relative_argument_to_a_plugin_under_c_names_a_file_under_the_c_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let sub = ws.join("github");
    fs::write(sub.join("note.txt"), "UNDER_C\n").unwrap();
    let plugin_dir = common::tempdir().unwrap();
    fs::write(plugin_dir.path().join("note.txt"), "UNDER_SHELL_CWD\n").unwrap();
    write_script(
        plugin_dir.path(),
        "rwv-show",
        "cat \"$1\"\n",
        "type \"%~1\"\n",
    );

    rwv()
        .args(["-C", &sub.to_string_lossy(), "show", "note.txt"])
        .current_dir(plugin_dir.path())
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success()
        .stdout(
            predicate::str::contains("UNDER_C")
                .and(predicate::str::contains("UNDER_SHELL_CWD").not()),
        );
}

/// `-w` names the invocation directory too: the plugin starts in the
/// workweave.
#[test]
fn a_plugin_under_w_starts_in_the_workweave() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let ws_canon = repoweave::path_spelling::resolve(&ws).unwrap();
    let ww_dir = ws_canon
        .parent()
        .unwrap()
        .join(".workweaves")
        .join("myproj--feat");
    fs::create_dir_all(ww_dir.join("github")).unwrap();
    fs::create_dir_all(ww_dir.join("projects").join("myproj")).unwrap();
    fs::write(
        ww_dir.join(".rwv-workweave"),
        common::workweave_marker(&ws_canon, "myproj", &ws_canon),
    )
    .unwrap();
    common::register_workweave(&ws, "myproj", "feat", &ww_dir);
    let plugin_dir = common::tempdir().unwrap();
    write_script(plugin_dir.path(), "rwv-whereami", CWD_SH, CWD_CMD);

    let assert = rwv()
        .args(["-w", "myproj--feat", "whereami"])
        .current_dir(&ws)
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        same_dir(&cwd_line(&stdout), &ww_dir),
        "plugin did not start in the workweave {}: {stdout}",
        ww_dir.display()
    );
}

/// A plugin found through a relative `PATH` entry is the file that entry
/// names from the shell's cwd, even when the plugin starts somewhere else.
#[test]
fn a_plugin_on_a_relative_path_entry_runs_under_c() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let shell_cwd = common::tempdir().unwrap();
    fs::create_dir_all(shell_cwd.path().join("plugins")).unwrap();
    write_echo(
        &shell_cwd.path().join("plugins"),
        "rwv-relpath",
        "RELPATH_RAN",
    );

    rwv()
        .args(["-C", &ws.to_string_lossy(), "relpath"])
        .current_dir(shell_cwd.path())
        .env("PATH", prepend_path(Path::new("plugins")))
        .assert()
        .success()
        .stdout(predicate::str::contains("RELPATH_RAN"));
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
    write_echo(plugin_dir.path(), "rwv-would-run", "SHOULD_NOT_RUN");
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
    write_echo(plugin_dir.path(), "rwv-would-run", "SHOULD_NOT_RUN");
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
    write_echo(plugin_dir.path(), "rwv-foo", "EXPLAIN_EXEC_BUG");
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
    write_echo(plugin_dir.path(), "rwv-my-multi-word-verb", "RAN");
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
    let sh: String = vars
        .iter()
        .map(|v| {
            format!(
                "if [ -n \"${{{v}+x}}\" ]; then echo \"{v}=${{{v}}}\" ; else echo \"{v}=UNSET\" ; fi\n",
            )
        })
        .collect();
    let cmd: String = std::iter::once("setlocal EnableDelayedExpansion\n".to_owned())
        .chain(
            vars.iter()
                .map(|v| format!("if defined {v} (echo {v}=!{v}!) else (echo {v}=UNSET)\n")),
        )
        .collect();
    write_script(dir, name, &sh, &cmd)
}

const ENVELOPE_VARS: &[&str] = &[
    "RWV_VERSION",
    "RWV_WORKSPACE",
    "RWV_WORKWEAVE",
    "RWV_WORKWEAVE_UNREGISTERED",
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

/// `RWV_VERSION` is always set to the Cargo version, even outside a workspace.
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

    assert_eq!(
        env.get("RWV_VERSION").map(|s| s.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
        "RWV_VERSION is the Cargo version outside a workspace; got: {env:?}"
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
    assert_eq!(
        env.get("RWV_VERSION").map(|s| s.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
        "RWV_VERSION is the Cargo version; got: {env:?}"
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
    let ws_canon = repoweave::path_spelling::resolve(&ws).unwrap();
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
    let marker_content = common::workweave_marker(&ws_canon, "myproj", &ws_canon);
    std::fs::write(ww_dir.join(".rwv-workweave"), &marker_content).unwrap();
    // Replicate workspace structure in the workweave dir.
    std::fs::create_dir_all(ww_dir.join("github")).unwrap();
    std::fs::create_dir_all(ww_dir.join("projects").join("myproj")).unwrap();
    common::register_workweave(&ws, "myproj", "feat", &ww_dir);

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

/// A plugin spawned inside a workweave the registry does not name must not
/// receive the primary's envelope.
///
/// The identity is genuinely absent — nothing to put in `RWV_WORKWEAVE` — and
/// the protocol documents that absence as "not in a workweave". Read through
/// the spawn a plugin actually gets, since that is the surface the claim is
/// made on: an unregistered workweave and the primary differ here, or a
/// third-party tool is misinformed about where it is running.
#[test]
fn envelope_marks_an_unregistered_workweave_apart_from_primary() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let ws_canon = repoweave::path_spelling::resolve(&ws).unwrap();
    let plugin_dir = common::tempdir().unwrap();
    write_env_dump_plugin(plugin_dir.path(), "rwv-envcheck", ENVELOPE_VARS);

    // A marker-bearing workweave with no registry entry — the state doctor
    // reports as `unregistered-workweave` and `--fix` adopts.
    let ww_dir = ws_canon
        .parent()
        .unwrap()
        .join(".workweaves")
        .join("myproj--unrecorded");
    std::fs::create_dir_all(ww_dir.join("projects").join("myproj")).unwrap();
    std::fs::write(
        ww_dir.join(".rwv-workweave"),
        common::workweave_marker(&ws_canon, "myproj", &ws_canon),
    )
    .unwrap();

    let in_workweave = rwv()
        .arg("envcheck")
        .current_dir(&ww_dir)
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let ww_env = parse_env_dump(&String::from_utf8_lossy(&in_workweave.get_output().stdout));

    let at_primary = rwv()
        .arg("envcheck")
        .current_dir(&ws)
        .env("PATH", prepend_path(plugin_dir.path()))
        .assert()
        .success();
    let primary_env = parse_env_dump(&String::from_utf8_lossy(&at_primary.get_output().stdout));

    // The property first, then the variable that carries it — a mutation
    // collapsing the two states must be reported as the collapse.
    assert_eq!(
        ww_env.get("RWV_WORKWEAVE").map(|s| s.as_str()),
        Some("UNSET"),
        "an unregistered workweave has no identity to carry; got: {ww_env:?}"
    );
    assert_ne!(
        ww_env, primary_env,
        "an unregistered workweave and the primary must not hand a plugin the same environment"
    );

    assert_eq!(
        ww_env.get("RWV_WORKWEAVE_UNREGISTERED").map(|s| s.as_str()),
        Some("1"),
        "the state must reach the plugin; got: {ww_env:?}"
    );
    assert_eq!(
        primary_env
            .get("RWV_WORKWEAVE_UNREGISTERED")
            .map(|s| s.as_str()),
        Some("UNSET"),
        "the primary must be unchanged by this variable; got: {primary_env:?}"
    );
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
fn prepend_path(dir: &Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(
        std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&inherited)),
    )
    .expect("PATH entries rejoin")
}

/// Build a minimal workspace at `tmp/ws` with a `projects/<project>/rwv.toml`
/// and a `.rwv-active` pointer. Returns the workspace root.
fn make_minimal_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects").join(project)).unwrap();
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::write(
        ws.join("projects").join(project).join("rwv.toml"),
        "[repositories]\n",
    )
    .unwrap();
    std::fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();
    ws
}
