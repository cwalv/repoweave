//! External-subcommand dispatch and discovery: `rwv <verb>` where `<verb>` is
//! not a core verb resolves to `rwv-<verb>` on `$PATH` and execs it, exit/signal
//! propagated verbatim.
//!
//! # Contract
//!
//! - **Core always wins.** clap parses core verbs before external fallthrough
//!   fires, so a `rwv-status` on `$PATH` can never shadow the builtin.
//!   This module is only reached for verbs clap did not match.
//! - **Addressing flags are consumed by rwv.** Global `-C`, `-w`, and the
//!   per-verb `--project` are the addressing surface for the workspace
//!   coordinate; the external verb never sees them in its argv.
//! - **Exit propagation is verbatim.** A normal exit propagates the child's
//!   status code; signal death (Unix) maps to the conventional `128 + N`
//!   exit and is reported on stderr as `rwv-<verb> terminated by signal N`.
//! - **Two error surfaces, no more.** Everything the dispatcher can go wrong
//!   on collapses to exactly one of: `unknown verb` (no core verb and no
//!   `rwv-<verb>` on `$PATH`) or `exec failure` (found but not spawnable,
//!   errno reported).
//! - **No output wrapping.** The child owns stdout and stderr entirely so
//!   plugins that emit JSON, drive a terminal, or stream progress work
//!   without translation.
//!
//! # Soft fallthrough
//!
//! When no addressing flag is given and the cwd walk finds no workspace,
//! the plugin is still spawned (some plugins legitimately run outside a
//! workspace — `--help`, generators). Explicit-flag resolution failure is
//! an rwv error before any spawn attempt: the user named a target that
//! does not exist and no plugin can salvage that.
//!
//! # PATH discovery
//!
//! Single-binary lookup goes through `which::which()`. Bulk discovery
//! (for `--help` and doctor) goes through `which::which_re_in` (the
//! `which` crate's regex-based multi-match). Both paths stay behind the
//! crate boundary — no explicit `std::env::var_os("PATH")` read appears
//! in this module: the `which` crate owns the OS executable-discovery
//! surface. The `regex` feature is enabled on the crate so
//! `which_re_in` is available.
//!
//! Duplicate handling: when the same `rwv-<verb>` name appears in
//! multiple `PATH` directories, first-found wins at exec time (standard
//! `PATH` semantics). `discover_plugins` returns every copy and marks all
//! but the first as `shadowed = true` so callers (help section, doctor)
//! can surface this for audit.
//!
//! # Env envelope
//!
//! Every spawn sets a `RWV_*` context envelope on the child process. The
//! envelope is a pure projection of the resolved [`Resolution`] value —
//! the same value the `--json` `resolution` block serializes — so both
//! surfaces are always consistent.
//!
//! | Variable | Value | Unset when |
//! |---|---|---|
//! | `RWV_VERSION` | `rwv` semver ([`crate::rwv_version`]) | never |
//! | `RWV_WORKSPACE` | primary workspace root (absolute path) | no workspace resolved |
//! | `RWV_WORKWEAVE` | `<project>--<name>` | not in / not addressing a workweave |
//! | `RWV_PROJECT` | resolved project name | no project resolved |
//!
//! Presence of `RWV_WORKWEAVE` encodes the checkout kind — no separate kind
//! variable is needed.
//!
//! `rwv` never reads any of these variables back. They are outputs set at
//! spawn for the child; a plugin that needs to address `rwv` explicitly
//! uses them as arguments:
//! `rwv -C "$RWV_WORKSPACE" --project "$RWV_PROJECT" status --json`.
//!
//! The seam for envelope injection is [`build_command`] — all spawn paths
//! go through it. Do not construct the child command inline.

use crate::workspace::Resolution;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;

/// Project a resolved [`Resolution`] into the `RWV_*` env-var envelope.
///
/// Returns a list of `(name, value)` pairs to set on the child process.
/// Variables that should be *unset* when no workspace is resolved are simply
/// absent from the list — callers set what is present and leave the rest
/// to the child's inherited env (which won't have them either).
///
/// `RWV_VERSION` is always included; workspace vars appear only when
/// `resolution` is `Some`.
///
/// This function is the single source of truth for which names are in the
/// envelope and how each [`Resolution`] field maps to them. Both the spawn
/// path ([`build_command`]) and tests use this function so a change to the
/// variable set is automatically reflected in both.
pub fn envelope_vars(resolution: Option<&Resolution>) -> Vec<(&'static str, String)> {
    let mut vars: Vec<(&'static str, String)> = Vec::new();
    vars.push(("RWV_VERSION", crate::rwv_version().to_owned()));
    if let Some(r) = resolution {
        vars.push(("RWV_WORKSPACE", r.workspace.to_string_lossy().into_owned()));
        if let Some(ww) = &r.workweave {
            vars.push(("RWV_WORKWEAVE", ww.clone()));
        }
        vars.push(("RWV_PROJECT", r.project.clone()));
    }
    vars
}

/// A discovered external command (`rwv-<verb>`) on `PATH`.
///
/// Records are sorted by `(name, path)` for deterministic output. When the
/// same name appears in more than one `PATH` directory, the first occurrence
/// wins at exec time; later occurrences are marked `shadowed = true` and carry
/// `shadowed_by` pointing at the winning binary.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct PluginRecord {
    /// Short verb name — the `<verb>` in `rwv-<verb>` and `rwv <verb>`.
    pub name: String,
    /// Absolute path of this binary on disk.
    pub path: String,
    /// `true` when another binary with the same name appears earlier in
    /// `PATH` and will be executed instead. This binary is unreachable
    /// via `rwv <name>` until the shadowing copy is removed.
    pub shadowed: bool,
    /// Absolute path of the binary that shadows this one. Present iff
    /// `shadowed` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<String>,
}

/// Discover every `rwv-*` executable on the given path string (a
/// `:`-separated list of directories, the same format as `$PATH`).
///
/// Results are sorted by `(name, path)` for deterministic output. Within the
/// same name, the first-found copy is the winner; subsequent copies are marked
/// `shadowed = true`.
///
/// Non-existent, non-directory, and permission-denied path entries are silently
/// skipped — this mirrors the OS PATH-walk behaviour and avoids spurious errors
/// when a `PATH` entry refers to an absent mount or `sudo`-only directory.
///
/// Non-executable files named `rwv-*` in a `PATH` directory are never returned
/// — the `which` crate enforces the executable bit just as `execve(2)` would.
///
/// `paths_override` is `None` to use the process's inherited `PATH` (via the
/// `which` crate's own PATH read — no explicit `std::env::var_os("PATH")`
/// appears in this module) or `Some(custom_path)` to search only the given
/// colon-separated directory list. Pass `Some(...)` in tests to pin the search
/// to fixture directories so tests are host-PATH-independent.
pub fn discover_plugins(paths_override: Option<&OsStr>) -> Vec<PluginRecord> {
    let re = regex::Regex::new(r"^rwv-.+").expect("constant pattern compiles");

    // Collect all matching executables. The `which` crate iterates PATH
    // directories in order and filters by executable bit — exactly the
    // exec-time semantics. Errors (no PATH, empty PATH, I/O) fold to an
    // empty iterator.
    //
    // Two dispatch paths:
    // - `which_re_in(re, Some(path))` when an override is provided (test mode).
    // - `which_re_in(re, env::var_os("PATH"))` via `which_re` for the production
    //   path. `which_re` is the crate's own public function that reads PATH
    //   internally; no explicit `std::env::var_os("PATH")` call appears here.
    let found: Vec<PathBuf> = match paths_override {
        Some(p) => match which::which_re_in(re, Some(p)) {
            Ok(it) => it.collect(),
            Err(_) => Vec::new(),
        },
        None => match which::which_re(re) {
            Ok(it) => it.collect(),
            Err(_) => Vec::new(),
        },
    };

    // Build name → path pairs in PATH order (the order `which_re_in` / `which_re`
    // returns them: directory order from $PATH, entries within a dir in readdir
    // order). The first occurrence of each name in this PATH-order list is the
    // exec-time winner — that determines the `shadowed` flag.
    let path_ordered: Vec<(String, PathBuf)> = found
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.strip_prefix("rwv-")?.to_owned();
            Some((name, p))
        })
        .collect();

    // Determine the winner (first occurrence in PATH order) for each name.
    let mut winner_paths: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (name, path) in &path_ordered {
        winner_paths
            .entry(name.clone())
            .or_insert_with(|| path.to_string_lossy().into_owned());
    }

    // Build the output records, then sort by (name, path) for deterministic
    // agent-facing output. Shadowing is determined by the winner_paths map
    // (PATH order), NOT by the sort order.
    let mut records: Vec<PluginRecord> = path_ordered
        .into_iter()
        .map(|(name, path)| {
            let path_str = path.to_string_lossy().into_owned();
            let winner = winner_paths.get(&name).cloned().unwrap_or_default();
            let shadowed = path_str != winner;
            PluginRecord {
                shadowed_by: if shadowed { Some(winner) } else { None },
                name,
                path: path_str,
                shadowed,
            }
        })
        .collect();

    // Stable sort by (name, path) — deterministic across hosts regardless of
    // readdir order, without disturbing the shadowing decisions made above.
    records.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));

    records
}

/// Build the text for the "External commands" section appended to `rwv --help`
/// output via `clap`'s `after_help`.
///
/// Returns `None` when no `rwv-*` executables are found on `PATH` — the section
/// is omitted entirely rather than showing an empty list.
///
/// `paths_override` is forwarded to [`discover_plugins`]; pass `None` to use
/// the process's inherited `PATH`, or `Some(custom_path)` in tests.
pub fn external_commands_help_section(paths_override: Option<&OsStr>) -> Option<String> {
    let records = discover_plugins(paths_override);
    // List only non-shadowed records in the help section — shadowed copies
    // are audit surface for doctor, not primary navigation for users.
    let names: Vec<&str> = records
        .iter()
        .filter(|r| !r.shadowed)
        .map(|r| r.name.as_str())
        .collect();
    if names.is_empty() {
        return None;
    }
    let mut out = String::from("External commands:\n");
    for name in names {
        out.push_str(&format!("  {name}\n"));
    }
    out.push_str("\nInvoke as `rwv <name>`. Run `rwv <name> --help` for each command's own usage.");
    Some(out)
}

/// Build the `Command` that will spawn `rwv-<verb>` for the given args.
///
/// Sets the `RWV_*` context envelope on the child before returning:
/// - `RWV_VERSION` is always set to the `rwv` semver.
/// - `RWV_WORKSPACE`, `RWV_WORKWEAVE` (when in a workweave), and
///   `RWV_PROJECT` are set when `resolution` is `Some`; they are absent
///   from the child's env when no workspace was resolved (soft fallthrough).
///
/// The envelope is derived from `resolution` via [`envelope_vars`] — the
/// same [`Resolution`] value the `--json` output block serializes. The two
/// surfaces (JSON and env) are projections of one value and are therefore
/// always consistent by construction.
///
/// The child inherits stdin, stdout, and stderr — [`std::process::Command`]
/// does that by default when none of `stdin`/`stdout`/`stderr` are set.
/// This preserves the external command's terminal control and its own I/O
/// contract.
///
/// All spawn paths must go through this function. Do not construct the child
/// command inline.
pub fn build_command(
    binary: &std::path::Path,
    args: &[OsString],
    resolution: Option<&Resolution>,
) -> Command {
    let mut cmd = Command::new(binary);
    cmd.args(args);
    for (name, value) in envelope_vars(resolution) {
        cmd.env(name, value);
    }
    cmd
}

/// Report an unknown verb — no core verb and no `rwv-<verb>` on `$PATH`.
///
/// Message is deliberately short: name the verb, name the two things we
/// checked, point at `rwv --help`. Agents parse the shape; humans read the
/// prose.
fn unknown_verb_error(verb: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "unknown verb `{verb}`: no core verb and no `rwv-{verb}` on `$PATH`. \
         Try `rwv --help` for the list of core verbs."
    )
}

/// Report an exec failure — a `rwv-<verb>` binary was found on `$PATH` but
/// could not be spawned. The OS error (permission denied, ENOEXEC, etc.)
/// is preserved so operators can diagnose without a second attempt.
fn exec_failure_error(verb: &str, binary: &std::path::Path, err: std::io::Error) -> anyhow::Error {
    anyhow::anyhow!("failed to exec `rwv-{verb}` ({}): {err}", binary.display(),)
}

/// Look up `rwv-<verb>` on `$PATH`.
///
/// Returns `None` when no such binary exists. Any other lookup failure
/// (permission denied while stat'ing an ancestor, etc.) is folded into
/// `None` — the caller reports "unknown verb" which is the correct
/// user-visible outcome (from the operator's perspective the binary is
/// not reachable).
fn find_plugin(verb: &str) -> Option<PathBuf> {
    let name = format!("rwv-{verb}");
    which::which(&name).ok()
}

/// Dispatch an external subcommand: locate `rwv-<verb>`, spawn it with
/// `args` and the context envelope, propagate its exit status. Never returns
/// on success — exits the process with the child's code. Returns an error
/// for the two rwv-side failure modes documented on the module.
///
/// `resolution` is the resolved workspace context; it is projected into the
/// `RWV_*` env-var envelope via [`build_command`]. Pass `None` when the cwd
/// walk found no workspace (soft fallthrough — `RWV_VERSION` is still set).
///
/// Signal death (Unix): mirrored to `128 + N` and reported on stderr. Exit
/// otherwise verbatim.
pub fn dispatch_external(
    verb: &str,
    args: &[OsString],
    resolution: Option<&Resolution>,
) -> anyhow::Result<std::convert::Infallible> {
    let binary = find_plugin(verb).ok_or_else(|| unknown_verb_error(verb))?;

    let mut cmd = build_command(&binary, args, resolution);
    let mut child = cmd
        .spawn()
        .map_err(|e| exec_failure_error(verb, &binary, e))?;

    // Wait; propagate. `wait()` inherits the child's I/O, so stdout/stderr
    // stream directly through this process without buffering.
    let status = child
        .wait()
        .map_err(|e| exec_failure_error(verb, &binary, e))?;

    if let Some(code) = status.code() {
        std::process::exit(code);
    }

    // No exit code → on Unix this means the child was terminated by a
    // signal. Report and mirror the conventional 128 + N mapping so
    // downstream consumers (shells, CI runners) see the standard
    // indication. Signals do not exist on Windows, so the branch is
    // Unix-only — release builds target windows-msvc too.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            eprintln!("rwv-{verb} terminated by signal {sig}");
            std::process::exit(128 + sig);
        }
    }

    // Neither an exit code nor a signal — should be impossible.
    // Emit a defensive 1 rather than looping forever.
    eprintln!("rwv-{verb} exited abnormally with no status");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;

    #[test]
    fn build_command_sets_program_and_args() {
        let binary = std::path::Path::new("/tmp/rwv-example");
        let args: Vec<OsString> = vec!["--flag".into(), "value".into(), "--".into(), "-x".into()];
        let cmd = build_command(binary, &args, None);
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/tmp/rwv-example"));
        let got_args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(
            got_args,
            vec![
                std::ffi::OsStr::new("--flag"),
                std::ffi::OsStr::new("value"),
                std::ffi::OsStr::new("--"),
                std::ffi::OsStr::new("-x"),
            ]
        );
    }

    #[test]
    fn envelope_vars_always_includes_version() {
        let vars = envelope_vars(None);
        let names: Vec<&str> = vars.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"RWV_VERSION"),
            "RWV_VERSION must always be present; got: {names:?}"
        );
        // Without a resolution, workspace vars must not appear.
        assert!(
            !names.contains(&"RWV_WORKSPACE"),
            "unexpected RWV_WORKSPACE"
        );
        assert!(
            !names.contains(&"RWV_WORKWEAVE"),
            "unexpected RWV_WORKWEAVE"
        );
        assert!(!names.contains(&"RWV_PROJECT"), "unexpected RWV_PROJECT");
    }

    #[test]
    fn envelope_vars_primary_checkout_no_workweave() {
        let r = crate::workspace::Resolution {
            workspace: std::path::PathBuf::from("/ws/primary"),
            workweave: None,
            project: "myproj".to_owned(),
        };
        let vars = envelope_vars(Some(&r));
        let map: std::collections::HashMap<_, _> = vars.into_iter().collect();
        assert_eq!(
            map.get("RWV_VERSION"),
            Some(&crate::rwv_version().to_owned())
        );
        assert_eq!(map.get("RWV_WORKSPACE"), Some(&"/ws/primary".to_owned()));
        assert!(
            !map.contains_key("RWV_WORKWEAVE"),
            "RWV_WORKWEAVE must be absent at primary"
        );
        assert_eq!(map.get("RWV_PROJECT"), Some(&"myproj".to_owned()));
    }

    #[test]
    fn envelope_vars_workweave_checkout_includes_workweave() {
        let r = crate::workspace::Resolution {
            workspace: std::path::PathBuf::from("/ws/primary"),
            workweave: Some("myproj--fo-123".to_owned()),
            project: "myproj".to_owned(),
        };
        let vars = envelope_vars(Some(&r));
        let map: std::collections::HashMap<_, _> = vars.into_iter().collect();
        assert_eq!(
            map.get("RWV_VERSION"),
            Some(&crate::rwv_version().to_owned())
        );
        assert_eq!(map.get("RWV_WORKSPACE"), Some(&"/ws/primary".to_owned()));
        assert_eq!(map.get("RWV_WORKWEAVE"), Some(&"myproj--fo-123".to_owned()));
        assert_eq!(map.get("RWV_PROJECT"), Some(&"myproj".to_owned()));
    }

    #[test]
    fn unknown_verb_error_names_the_verb_and_path() {
        let err = unknown_verb_error("frobnicate").to_string();
        assert!(err.contains("frobnicate"), "err: {err}");
        assert!(err.contains("rwv-frobnicate"), "err: {err}");
        assert!(err.contains("PATH"), "err: {err}");
        assert!(err.contains("rwv --help"), "err: {err}");
    }

    #[test]
    fn exec_failure_error_carries_errno_prose() {
        let io_err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let err = exec_failure_error("example", std::path::Path::new("/opt/rwv-example"), io_err)
            .to_string();
        assert!(err.contains("rwv-example"), "err: {err}");
        assert!(err.contains("/opt/rwv-example"), "err: {err}");
        // io::Error's Display carries the errno-derived prose ("permission denied").
        assert!(
            err.to_lowercase().contains("permission denied"),
            "err: {err}"
        );
    }

    /// find_plugin's "not on PATH" branch — the negative case, no PATH state
    /// mutation involved. The verb is a nonsense name so `rwv-<verb>` is
    /// vanishingly unlikely to exist on any host.
    #[test]
    fn find_plugin_returns_none_for_missing() {
        assert!(find_plugin("this-verb-definitely-does-not-exist-xyz-42").is_none());
    }

    // -------------------------------------------------------------------------
    // Test helpers for discovery tests
    // -------------------------------------------------------------------------

    // The fixtures below are `#!/bin/sh` scripts made executable with a mode
    // bit, and the subject under test is which of them `which` will dispatch.
    // Windows has no executable bit: `which` selects on PATHEXT there, so a
    // discoverable plugin is `rwv-foo.exe`, and `strip_prefix("rwv-")` would
    // name the verb `foo.exe`. What these assert is therefore not merely
    // spelled Unix-ly, it is undefined on Windows until that naming is
    // decided.
    /// Create an executable file at `dir/name`. Returns the file path.
    #[cfg(unix)]
    fn make_executable(dir: &std::path::Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        fs::write(&p, "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// Create a non-executable file at `dir/name` (mode 0o644).
    #[cfg(unix)]
    fn make_non_executable(dir: &std::path::Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        fs::write(&p, "data\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// Build an OsString PATH from a list of directories.
    #[cfg(unix)]
    fn join_paths(dirs: &[&std::path::Path]) -> OsString {
        std::env::join_paths(dirs).expect("join paths")
    }

    // -------------------------------------------------------------------------
    // discover_plugins tests
    // -------------------------------------------------------------------------

    /// Empty PATH (no dirs) → empty result, no panic.
    #[test]
    fn discover_plugins_empty_path_is_empty() {
        let result = discover_plugins(Some(OsStr::new("")));
        assert!(result.is_empty(), "expected empty, got {result:?}");
    }

    /// A single fixture dir with two executable `rwv-*` files.
    #[test]
    #[cfg(unix)]
    fn discover_plugins_finds_executables_in_single_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        make_executable(dir, "rwv-bar");
        make_executable(dir, "rwv-foo");
        let path = join_paths(&[dir]);
        let result = discover_plugins(Some(path.as_os_str()));
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        // sorted by name
        assert_eq!(names, &["bar", "foo"], "names: {names:?}");
        for r in &result {
            assert!(!r.shadowed, "should not be shadowed: {r:?}");
            assert!(r.shadowed_by.is_none(), "shadowed_by should be None: {r:?}");
        }
    }

    /// Non-executable `rwv-*` files must NOT appear in results.
    #[test]
    #[cfg(unix)]
    fn discover_plugins_skips_non_executable_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        make_non_executable(dir, "rwv-notexec");
        make_executable(dir, "rwv-isexec");
        let path = join_paths(&[dir]);
        let result = discover_plugins(Some(path.as_os_str()));
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, &["isexec"]);
    }

    /// Non-existent PATH dir is silently skipped.
    #[test]
    #[cfg(unix)]
    fn discover_plugins_nonexistent_path_dir_silently_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        make_executable(dir, "rwv-present");
        let absent = tmp.path().join("does_not_exist");
        let path = join_paths(&[dir, &absent]);
        let result = discover_plugins(Some(path.as_os_str()));
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, &["present"]);
    }

    /// Files not named `rwv-*` are not returned.
    #[test]
    #[cfg(unix)]
    fn discover_plugins_ignores_non_rwv_executables() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        make_executable(dir, "git");
        make_executable(dir, "cargo");
        make_executable(dir, "rwv-mine");
        let path = join_paths(&[dir]);
        let result = discover_plugins(Some(path.as_os_str()));
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, &["mine"]);
    }

    /// Same name in two PATH dirs: first dir wins; second is shadowed.
    ///
    /// Results are sorted by (name, path) for deterministic output, so the
    /// position of the winner in the vec depends on path lexicography. We
    /// key on the `shadowed` flag, not position.
    #[test]
    #[cfg(unix)]
    fn discover_plugins_duplicate_shadowed_by_path_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir1 = tmp.path().join("dir1");
        let dir2 = tmp.path().join("dir2");
        fs::create_dir_all(&dir1).unwrap();
        fs::create_dir_all(&dir2).unwrap();
        let winner = make_executable(&dir1, "rwv-foo");
        make_executable(&dir2, "rwv-foo");
        let path = join_paths(&[dir1.as_path(), dir2.as_path()]);
        let result = discover_plugins(Some(path.as_os_str()));
        assert_eq!(result.len(), 2, "expected 2 records, got {result:?}");
        // Both are named "foo".
        assert!(result.iter().all(|r| r.name == "foo"), "{result:?}");
        // Exactly one winner (not shadowed) and one shadowed.
        let winner_rec = result.iter().find(|r| !r.shadowed).expect("winner");
        let shadowed_rec = result.iter().find(|r| r.shadowed).expect("shadowed");
        assert_eq!(
            winner_rec.path,
            winner.to_string_lossy().as_ref(),
            "wrong winner path: {winner_rec:?}"
        );
        assert_eq!(
            shadowed_rec.shadowed_by.as_deref(),
            Some(winner.to_string_lossy().as_ref()),
            "shadowed_by should point to winner: {shadowed_rec:?}"
        );
    }

    /// Same name in THREE PATH dirs: only the first (PATH-order winner) is
    /// unshadowed; the other two carry shadowed_by pointing at the winner.
    #[test]
    #[cfg(unix)]
    fn discover_plugins_triple_duplicate_all_after_first_shadowed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir1 = tmp.path().join("a");
        let dir2 = tmp.path().join("b");
        let dir3 = tmp.path().join("c");
        for d in &[&dir1, &dir2, &dir3] {
            fs::create_dir_all(d).unwrap();
        }
        let winner = make_executable(&dir1, "rwv-dup");
        make_executable(&dir2, "rwv-dup");
        make_executable(&dir3, "rwv-dup");
        let path = join_paths(&[dir1.as_path(), dir2.as_path(), dir3.as_path()]);
        let result = discover_plugins(Some(path.as_os_str()));
        assert_eq!(result.len(), 3, "expected 3 records, got {result:?}");
        // Exactly one winner.
        let winners: Vec<&PluginRecord> = result.iter().filter(|r| !r.shadowed).collect();
        assert_eq!(winners.len(), 1, "exactly one winner: {result:?}");
        assert_eq!(
            winners[0].path,
            winner.to_string_lossy().as_ref(),
            "wrong winner: {result:?}"
        );
        // Two shadowed, both pointing at the winner.
        let winner_str = winner.to_string_lossy().into_owned();
        let shadowed: Vec<&PluginRecord> = result.iter().filter(|r| r.shadowed).collect();
        assert_eq!(shadowed.len(), 2, "two shadowed: {result:?}");
        for s in &shadowed {
            assert_eq!(
                s.shadowed_by.as_deref(),
                Some(winner_str.as_str()),
                "shadowed_by should point at winner: {s:?}"
            );
        }
    }

    /// Symlinked executable `rwv-*` is included.
    #[test]
    #[cfg(unix)]
    fn discover_plugins_symlinked_plugin_is_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin_dir = tmp.path().join("real");
        let link_dir = tmp.path().join("links");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&link_dir).unwrap();
        let real_bin = make_executable(&bin_dir, "rwv-sym");
        std::os::unix::fs::symlink(&real_bin, link_dir.join("rwv-sym")).unwrap();
        let path = join_paths(&[link_dir.as_path()]);
        let result = discover_plugins(Some(path.as_os_str()));
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            &["sym"],
            "symlink should be discoverable: {result:?}"
        );
        assert!(!result[0].shadowed);
    }

    /// Output is sorted by name across multiple dirs.
    #[test]
    #[cfg(unix)]
    fn discover_plugins_result_sorted_by_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        make_executable(dir, "rwv-zebra");
        make_executable(dir, "rwv-alpha");
        make_executable(dir, "rwv-middle");
        let path = join_paths(&[dir]);
        let result = discover_plugins(Some(path.as_os_str()));
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, &["alpha", "middle", "zebra"]);
    }

    // -------------------------------------------------------------------------
    // external_commands_help_section tests
    // -------------------------------------------------------------------------

    /// Empty PATH → section absent (`None` returned).
    #[test]
    fn help_section_absent_when_no_plugins() {
        let section = external_commands_help_section(Some(OsStr::new("")));
        assert!(section.is_none(), "expected None, got {section:?}");
    }

    /// With a fixture plugin, the help section lists the name.
    #[test]
    #[cfg(unix)]
    fn help_section_lists_plugin_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        make_executable(dir, "rwv-example");
        let path = join_paths(&[dir]);
        let section = external_commands_help_section(Some(path.as_os_str()));
        let text = section.expect("should produce a section");
        assert!(
            text.contains("External commands"),
            "section header missing: {text}"
        );
        assert!(text.contains("example"), "plugin name missing: {text}");
        assert!(
            text.contains("rwv <name>") || text.contains("rwv-"),
            "invocation hint missing: {text}"
        );
    }

    /// Shadowed duplicates are NOT listed in the help section (only unique names).
    #[test]
    #[cfg(unix)]
    fn help_section_omits_shadowed_duplicates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir1 = tmp.path().join("d1");
        let dir2 = tmp.path().join("d2");
        fs::create_dir_all(&dir1).unwrap();
        fs::create_dir_all(&dir2).unwrap();
        make_executable(&dir1, "rwv-tool");
        make_executable(&dir2, "rwv-tool");
        let path = join_paths(&[dir1.as_path(), dir2.as_path()]);
        let section =
            external_commands_help_section(Some(path.as_os_str())).expect("section present");
        // Count occurrences of "tool" in the section — should appear exactly once
        let count = section.matches("tool").count();
        assert_eq!(
            count, 1,
            "expected 'tool' exactly once in help, got:\n{section}"
        );
    }
}
