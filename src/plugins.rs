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
//! - **The plugin starts in the invocation directory.** That is the `-C`
//!   directory, the `-w` workweave, or else the process cwd — the directory
//!   a core verb resolves relative path arguments against — so a plugin's own
//!   relative arguments mean what they would to a core verb, as under
//!   `git -C`.
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
//! Discovery lists exactly what dispatch would run. The verb is the name
//! typed: on Windows the `PATHEXT` extension that made the file executable
//! is dropped (`rwv-foo.EXE` lists as `foo`) and a file whose extension is
//! not in `PATHEXT` is not a plugin; on Unix nothing is dropped, because the
//! lookup appends nothing (`rwv-foo.py` lists as `foo.py`).
//!
//! Duplicate handling: when one verb resolves to several files, the copy
//! dispatch picks wins — the earliest `PATH` directory, and within one
//! directory the earliest `PATHEXT` entry. `discover_plugins` returns every
//! copy and marks all but the winner `shadowed = true` so callers (help
//! section, doctor) can surface this for audit.
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
//! | `RWV_WORKWEAVE` | `<project>--<name>` | not in a workweave, or in one the registry does not name |
//! | `RWV_WORKWEAVE_UNREGISTERED` | `1` | anywhere else |
//! | `RWV_PROJECT` | resolved project name | no project resolved |
//!
//! The checkout is one of three states and `RWV_WORKWEAVE` alone carries two
//! of them. A workweave the registry does not name has no identity to put in
//! it, so it is unset there — which is also how the primary looks.
//! `RWV_WORKWEAVE_UNREGISTERED` is what tells those two apart, and it is set
//! in that one case only, so a plugin that never looks at it sees exactly the
//! environment it saw before the variable existed.
//!
//! `rwv` never reads any of these variables back. They are outputs set at
//! spawn for the child; a plugin that needs to address `rwv` explicitly
//! uses them as arguments:
//! `rwv -C "$RWV_WORKSPACE" status --json --project "$RWV_PROJECT"`.
//!
//! The seam for envelope injection is `build_command` — all spawn paths
//! go through it. Do not construct the child command inline.

use crate::path_spelling::WirePath;
use crate::tool_program::ToolProgram;
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
/// path (`build_command`) and tests use this function so a change to the
/// variable set is automatically reflected in both.
pub fn envelope_vars(resolution: Option<&Resolution>) -> Vec<(&'static str, String)> {
    let mut vars: Vec<(&'static str, String)> = Vec::new();
    vars.push(("RWV_VERSION", crate::rwv_version().to_string()));
    if let Some(r) = resolution {
        vars.push(("RWV_WORKSPACE", r.workspace.as_str().to_owned()));
        if let Some(ww) = &r.workweave {
            vars.push(("RWV_WORKWEAVE", ww.clone()));
        }
        if r.workweave_unregistered {
            vars.push(("RWV_WORKWEAVE_UNREGISTERED", "1".to_owned()));
        }
        vars.push(("RWV_PROJECT", r.project.to_string()));
    }
    vars
}

/// A discovered external command (`rwv-<verb>`) on `PATH`.
///
/// Records are sorted by `(name, path)` for deterministic output. When several
/// files dispatch as the same name, the one dispatch runs wins; the others are
/// marked `shadowed = true` and carry `shadowed_by` pointing at the winning
/// binary.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct PluginRecord {
    /// The verb as typed in `rwv <verb>`: the file name after `rwv-`, less
    /// the `PATHEXT` extension on Windows.
    pub name: String,
    /// Absolute path of this binary on disk, carrying its real file name.
    pub path: WirePath,
    /// `true` when another binary with the same name will be executed
    /// instead: one earlier in `PATH`, or on Windows one in the same directory
    /// whose extension comes earlier in `PATHEXT`. This binary is unreachable
    /// via `rwv <name>` until the shadowing copy is removed.
    pub shadowed: bool,
    /// Absolute path of the binary that shadows this one. Present iff
    /// `shadowed` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<WirePath>,
}

/// How the host's executable lookup turns a typed name into a file name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LookupNaming {
    /// The typed name is the file name: an executable bit selects candidates.
    Verbatim,
    /// The lookup tries the bare name, then each `PATHEXT` extension in order,
    /// matched case-insensitively.
    PathExt(Vec<String>),
}

impl LookupNaming {
    fn host() -> Self {
        if cfg!(windows) {
            // `which` reads PATHEXT the same way and treats it unset as empty.
            Self::from_pathext(&std::env::var("PATHEXT").unwrap_or_default())
        } else {
            Self::Verbatim
        }
    }

    /// Parse a `PATHEXT` value as `which` does: `;`-separated, segments not
    /// starting with `.` dropped.
    fn from_pathext(raw: &str) -> Self {
        Self::PathExt(
            raw.split(';')
                .filter(|s| s.starts_with('.'))
                .map(str::to_owned)
                .collect(),
        )
    }

    /// The verb `file_name` dispatches as, and its rank among same-directory
    /// files for that verb (lower wins). `None` when the lookup never resolves
    /// a typed verb to this file.
    fn verb_of<'a>(&self, file_name: &'a str) -> Option<(&'a str, usize)> {
        let rest = file_name.strip_prefix("rwv-")?;
        let (verb, rank) = match self {
            Self::Verbatim => (rest, 0),
            Self::PathExt(exts) => match std::path::Path::new(file_name).extension() {
                None => (rest, 0),
                Some(ext) => {
                    let ext = ext.to_str()?;
                    let pos = exts.iter().position(|e| e[1..].eq_ignore_ascii_case(ext))?;
                    (&rest[..rest.len() - ext.len() - 1], pos + 1)
                }
            },
        };
        (!verb.is_empty()).then_some((verb, rank))
    }
}

/// Discover every `rwv-*` executable on the given path string (the same
/// format as `$PATH`).
///
/// Results are sorted by `(name, path)` for deterministic output. Within the
/// same name, the copy dispatch would run is the winner; every other copy is
/// marked `shadowed = true`.
///
/// Non-existent, non-directory, and permission-denied path entries are silently
/// skipped — this mirrors the OS PATH-walk behavior and avoids spurious errors
/// when a `PATH` entry refers to an absent mount or `sudo`-only directory.
///
/// A file named `rwv-*` that the platform would not execute is never returned:
/// on Unix the `which` crate enforces the executable bit just as `execve(2)`
/// would; on Windows a file must carry a `PATHEXT` extension.
///
/// `paths_override` is `None` to use the process's inherited `PATH` (via the
/// `which` crate's own PATH read — no explicit `std::env::var_os("PATH")`
/// appears in this module) or `Some(custom_path)` to search only the given
/// directory list. Pass `Some(...)` in tests to pin the search to fixture
/// directories so tests are host-PATH-independent.
pub fn discover_plugins(paths_override: Option<&OsStr>) -> Vec<PluginRecord> {
    let re = regex::Regex::new(r"^rwv-.+").expect("constant pattern compiles");

    // `which_re_in` yields matches directory by directory in PATH order,
    // readdir order within a directory. Errors (no PATH, empty PATH, I/O)
    // fold to an empty iterator.
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
    plugin_records(found, &LookupNaming::host())
}

/// Name and rank `found` — matches in PATH-directory order — under `naming`.
fn plugin_records(found: Vec<PathBuf>, naming: &LookupNaming) -> Vec<PluginRecord> {
    let mut dir_index = 0usize;
    let mut prev_dir: Option<PathBuf> = None;
    struct Candidate {
        dir_index: usize,
        rank: usize,
        name: String,
        path: PathBuf,
    }
    let mut ranked: Vec<Candidate> = Vec::new();
    for p in found {
        let dir = p.parent().map(PathBuf::from);
        if prev_dir.is_some() && prev_dir != dir {
            dir_index += 1;
        }
        prev_dir = dir;
        let Some(file_name) = p.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let Some((verb, rank)) = naming.verb_of(file_name) else {
            continue;
        };
        ranked.push(Candidate {
            dir_index,
            rank,
            name: verb.to_owned(),
            path: p,
        });
    }
    // Dispatch order: directory first, then PATHEXT position within it.
    ranked.sort_by_key(|c| (c.dir_index, c.rank));

    let mut winner_paths: std::collections::HashMap<String, PathBuf> =
        std::collections::HashMap::new();
    for c in &ranked {
        winner_paths
            .entry(c.name.clone())
            .or_insert_with(|| c.path.clone());
    }

    let mut records: Vec<PluginRecord> = ranked
        .into_iter()
        .map(|Candidate { name, path, .. }| {
            let winner = winner_paths.get(&name).cloned().unwrap_or_default();
            let shadowed = path != winner;
            PluginRecord {
                shadowed_by: shadowed.then(|| WirePath::of(&winner)),
                name,
                path: WirePath::of(&path),
                shadowed,
            }
        })
        .collect();

    // Shadowing was decided in dispatch order above; this sort is for output.
    records.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.path.as_str().cmp(b.path.as_str()))
    });

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
/// - `RWV_WORKSPACE`, `RWV_WORKWEAVE` (when the registry names the
///   workweave), `RWV_WORKWEAVE_UNREGISTERED` (when it does not), and
///   `RWV_PROJECT` are set when `resolution` is `Some`; they are absent
///   from the child's env when no workspace was resolved (soft fallthrough).
///
/// The envelope is derived from `resolution` via [`envelope_vars`] — the
/// same [`Resolution`] value the `--json` output block serializes. The two
/// surfaces (JSON and env) are projections of one value and are therefore
/// always consistent by construction.
///
/// The child starts in `invocation_dir`, the directory the invocation's
/// relative path arguments resolve against.
///
/// The child inherits stdin, stdout, and stderr — [`std::process::Command`]
/// does that by default when none of `stdin`/`stdout`/`stderr` are set.
/// This preserves the external command's terminal control and its own I/O
/// contract.
///
/// All spawn paths must go through this function. Do not construct the child
/// command inline.
pub(crate) fn build_command(
    plugin: &ToolProgram,
    args: &[OsString],
    invocation_dir: &std::path::Path,
    resolution: Option<&Resolution>,
) -> Command {
    let mut cmd = plugin.command();
    // The plugin gets its working directory in the spelling a shell would hold,
    // never the verbatim `\\?\` form a recorded `-w` path can carry.
    cmd.args(args)
        .current_dir(dunce::simplified(invocation_dir));
    // An inherited `PWD` names the shell's directory, not the one the plugin
    // starts in.
    #[cfg(unix)]
    cmd.env("PWD", invocation_dir);
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
    crate::refusal::refusal(
        crate::refusal::RefusalKind::UnknownVerb,
        format!(
            "unknown verb `{verb}`: no core verb and no `rwv-{verb}` on `$PATH`. \
             Try `rwv --help` for the list of core verbs."
        ),
    )
}

/// Report an exec failure — a `rwv-<verb>` binary was found on `$PATH` but
/// could not be spawned. The OS error (permission denied, ENOEXEC, etc.)
/// is preserved so operators can diagnose without a second attempt.
fn exec_failure_error(verb: &str, plugin: &ToolProgram, err: std::io::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "failed to exec `rwv-{verb}` ({}): {err:#}",
        crate::path_spelling::operator_path(plugin.path()),
    )
}

/// Look up `rwv-<verb>` on `$PATH`.
///
/// Returns `None` when no such binary exists. Any other lookup failure
/// (permission denied while stat'ing an ancestor, etc.) is folded into
/// `None` — the caller reports "unknown verb" which is the correct
/// user-visible outcome (from the operator's perspective the binary is
/// not reachable).
fn find_plugin(verb: &str) -> Option<ToolProgram> {
    ToolProgram::find(&format!("rwv-{verb}"))
}

/// Dispatch an external subcommand: locate `rwv-<verb>`, spawn it with
/// `args` and the context envelope, propagate its exit status. Never returns
/// on success — exits the process with the child's code. Returns an error
/// for the two rwv-side failure modes documented on the module.
///
/// `resolution` is the resolved workspace context; it is projected into the
/// `RWV_*` env-var envelope via `build_command`. Pass `None` when the cwd
/// walk found no workspace (soft fallthrough — `RWV_VERSION` is still set).
///
/// Signal death (Unix): mirrored to `128 + N` and reported on stderr. Exit
/// otherwise verbatim.
pub fn dispatch_external(
    verb: &str,
    args: &[OsString],
    invocation_dir: &std::path::Path,
    resolution: Option<&Resolution>,
) -> anyhow::Result<std::convert::Infallible> {
    let plugin = find_plugin(verb).ok_or_else(|| unknown_verb_error(verb))?;

    let mut cmd = build_command(&plugin, args, invocation_dir, resolution);
    leave_interrupts_to_the_plugin();
    let mut child = cmd
        .spawn()
        .map_err(|e| exec_failure_error(verb, &plugin, e))?;

    // Wait; propagate. `wait()` inherits the child's I/O, so stdout/stderr
    // stream directly through this process without buffering.
    let status = child
        .wait()
        .map_err(|e| exec_failure_error(verb, &plugin, e))?;

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

/// A terminal interrupt reaches the plugin and this process alike, the two
/// being one foreground job. Left at its default this process would exit at
/// once — handing the prompt back while the plugin is still winding down, and
/// discarding the status the plugin then exits with.
///
/// Called before the spawn, so no interrupt can find this process between the
/// plugin starting and the disposition changing. A handler that does nothing,
/// not `SIG_IGN`: exec resets a handled signal to its default and keeps an
/// ignored one ignored, so the plugin starts with the default disposition
/// only because this one is handled. A disposition, unlike a signal mask,
/// holds for every thread of this process.
#[cfg(unix)]
fn leave_interrupts_to_the_plugin() {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
    extern "C" fn outlive_the_interrupt(_: std::ffi::c_int) {}
    let action = SigAction::new(
        SigHandler::Handler(outlive_the_interrupt),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    for interrupt in [Signal::SIGINT, Signal::SIGQUIT] {
        // SAFETY: the handler touches nothing, so it is async-signal-safe.
        let _ = unsafe { sigaction(interrupt, &action) };
    }
}

/// The Windows console delivers Ctrl-C to every process attached to it; a
/// handler that returns without acting keeps this one alive until the plugin
/// exits. Called before the spawn for the reason the Unix one is. A handler
/// routine, unlike ignoring Ctrl-C outright, is not inherited by processes
/// started later.
#[cfg(windows)]
fn leave_interrupts_to_the_plugin() {
    let _ = ctrlc::set_handler(|| {});
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn build_command_sets_program_and_args() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = installed_plugin(tmp.path(), "rwv-example");
        let args: Vec<OsString> = vec!["--flag".into(), "value".into(), "--".into(), "-x".into()];
        let cmd = build_command(&plugin, &args, std::path::Path::new("/tmp"), None);
        assert_eq!(cmd.get_program(), plugin.path().as_os_str());
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
            workspace: crate::path_spelling::WirePath::of(std::path::Path::new("/ws/primary")),
            workweave: None,
            workweave_unregistered: false,
            project: crate::manifest::ProjectName::new("myproj").unwrap(),
        };
        let vars = envelope_vars(Some(&r));
        let map: std::collections::HashMap<_, _> = vars.into_iter().collect();
        assert_eq!(
            map.get("RWV_VERSION"),
            Some(&env!("CARGO_PKG_VERSION").to_owned())
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
            workspace: crate::path_spelling::WirePath::of(std::path::Path::new("/ws/primary")),
            workweave: Some("myproj--fo-123".to_owned()),
            workweave_unregistered: false,
            project: crate::manifest::ProjectName::new("myproj").unwrap(),
        };
        let vars = envelope_vars(Some(&r));
        let map: std::collections::HashMap<_, _> = vars.into_iter().collect();
        assert_eq!(
            map.get("RWV_VERSION"),
            Some(&env!("CARGO_PKG_VERSION").to_owned())
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
        let tmp = tempfile::tempdir().unwrap();
        let plugin = installed_plugin(tmp.path(), "rwv-example");
        let err = exec_failure_error("example", &plugin, io_err).to_string();
        assert!(err.contains("rwv-example"), "err: {err}");
        assert!(
            err.contains(&crate::path_spelling::operator_path(plugin.path())),
            "err: {err}"
        );
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

    /// Create a file at `dir/<name>` the host would execute, and return its
    /// path: a `#!/bin/sh` script with the exec bit on Unix, `<name>.cmd` on
    /// Windows, where `PATHEXT` plays the exec bit's part.
    fn make_executable(dir: &std::path::Path, name: &str) -> PathBuf {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join(name);
            fs::write(&p, "#!/bin/sh\necho hi\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
        #[cfg(windows)]
        {
            let p = dir.join(format!("{name}.cmd"));
            fs::write(&p, "@echo hi\r\n").unwrap();
            p
        }
    }

    /// A plugin installed in `dir` and found there, the way dispatch finds one.
    fn installed_plugin(dir: &std::path::Path, name: &str) -> ToolProgram {
        make_executable(dir, name);
        ToolProgram::find_in(name, dir.as_os_str(), dir).expect("the plugin is on its own PATH")
    }

    /// Create a file at `dir/<name>` the host would not execute: mode 0o644
    /// on Unix, `<name>.txt` on Windows.
    fn make_non_executable(dir: &std::path::Path, name: &str) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join(name);
            fs::write(&p, "data\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        }
        #[cfg(windows)]
        fs::write(dir.join(format!("{name}.txt")), "data\r\n").unwrap();
    }

    /// Build an OsString PATH from a list of directories.
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
        let winner_wire = crate::path_spelling::wire_path(&winner);
        assert_eq!(
            winner_rec.path.as_str(),
            winner_wire,
            "wrong winner path: {winner_rec:?}"
        );
        assert_eq!(
            shadowed_rec.shadowed_by.as_ref().map(WirePath::as_str),
            Some(winner_wire.as_str()),
            "shadowed_by should point to winner: {shadowed_rec:?}"
        );
    }

    /// Same name in THREE PATH dirs: only the first (PATH-order winner) is
    /// unshadowed; the other two carry shadowed_by pointing at the winner.
    #[test]
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
        let winner_str = crate::path_spelling::wire_path(&winner);
        assert_eq!(
            winners[0].path.as_str(),
            winner_str,
            "wrong winner: {result:?}"
        );
        // Two shadowed, both pointing at the winner.
        let shadowed: Vec<&PluginRecord> = result.iter().filter(|r| r.shadowed).collect();
        assert_eq!(shadowed.len(), 2, "two shadowed: {result:?}");
        for s in &shadowed {
            assert_eq!(
                s.shadowed_by.as_ref().map(WirePath::as_str),
                Some(winner_str.as_str()),
                "shadowed_by should point at winner: {s:?}"
            );
        }
    }

    /// Symlinked executable `rwv-*` is included.
    #[test]
    fn discover_plugins_symlinked_plugin_is_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin_dir = tmp.path().join("real");
        let link_dir = tmp.path().join("links");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&link_dir).unwrap();
        let real_bin = make_executable(&bin_dir, "rwv-sym");
        let link = link_dir.join(real_bin.file_name().unwrap());
        crate::symlink::create(&real_bin, &link, crate::symlink::LinkTarget::File).unwrap();
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

    // -------------------------------------------------------------------------
    // Naming and tie-break under each lookup convention
    // -------------------------------------------------------------------------

    fn windows_default_pathext() -> LookupNaming {
        LookupNaming::from_pathext(".COM;.EXE;.BAT;.CMD")
    }

    fn names_and_paths(records: &[PluginRecord]) -> Vec<(&str, &str)> {
        records
            .iter()
            .map(|r| (r.name.as_str(), r.path.as_str()))
            .collect()
    }

    #[test]
    fn pathext_parse_keeps_dotted_segments_in_order() {
        assert_eq!(
            LookupNaming::from_pathext(".COM;.exe;;BAT;.Cmd"),
            LookupNaming::PathExt(vec![".COM".into(), ".exe".into(), ".Cmd".into()])
        );
    }

    /// The listed verb is the one typed, whatever case the extension is in;
    /// the record's path keeps the real file name.
    #[test]
    fn pathext_naming_strips_the_extension_case_insensitively() {
        let found = vec![
            PathBuf::from("/d/rwv-foo.EXE"),
            PathBuf::from("/d/rwv-bar.cmd"),
            PathBuf::from("/d/rwv-baz.Bat"),
        ];
        let records = plugin_records(found, &windows_default_pathext());
        assert_eq!(
            names_and_paths(&records),
            vec![
                (
                    "bar",
                    crate::path_spelling::wire_path(std::path::Path::new("/d/rwv-bar.cmd"))
                        .as_str()
                ),
                (
                    "baz",
                    crate::path_spelling::wire_path(std::path::Path::new("/d/rwv-baz.Bat"))
                        .as_str()
                ),
                (
                    "foo",
                    crate::path_spelling::wire_path(std::path::Path::new("/d/rwv-foo.EXE"))
                        .as_str()
                ),
            ]
        );
    }

    /// A file whose extension is not in `PATHEXT` is the Windows counterpart
    /// of a file without the exec bit; an extensionless file `which` accepted
    /// is an executable image and keeps its whole name.
    #[test]
    fn pathext_naming_lists_only_what_pathext_makes_executable() {
        let found = vec![
            PathBuf::from("/d/rwv-notes.txt"),
            PathBuf::from("/d/rwv-native"),
            PathBuf::from("/d/rwv-.exe"),
        ];
        let records = plugin_records(found, &windows_default_pathext());
        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["native"]);
    }

    /// Unix lookup appends no extension, so `rwv foo` never reaches
    /// `rwv-foo.py`; the verb that does is `foo.py`, and listing must say so.
    #[test]
    fn verbatim_naming_strips_no_extension() {
        let found = vec![
            PathBuf::from("/d/rwv-foo.py"),
            PathBuf::from("/d/rwv-bar.exe"),
            PathBuf::from("/d/rwv-baz.txt"),
        ];
        let records = plugin_records(found, &LookupNaming::Verbatim);
        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["bar.exe", "baz.txt", "foo.py"]);
    }

    /// Two files for one verb in one directory: dispatch tries `PATHEXT` in
    /// order, so `.EXE` beats `.CMD` even when readdir yields `.cmd` first.
    #[test]
    fn pathext_tie_in_one_directory_goes_to_the_earlier_pathext_entry() {
        let cmd = PathBuf::from("/d/rwv-foo.cmd");
        let exe = PathBuf::from("/d/rwv-foo.exe");
        let records = plugin_records(vec![cmd.clone(), exe.clone()], &windows_default_pathext());
        let exe_wire = crate::path_spelling::wire_path(&exe);
        let cmd_wire = crate::path_spelling::wire_path(&cmd);
        let winner = records.iter().find(|r| !r.shadowed).expect("a winner");
        assert_eq!(winner.path.as_str(), exe_wire, "{records:?}");
        let loser = records
            .iter()
            .find(|r| r.shadowed)
            .expect("a shadowed copy");
        assert_eq!(loser.path.as_str(), cmd_wire, "{records:?}");
        assert_eq!(
            loser.shadowed_by.as_ref().map(WirePath::as_str),
            Some(exe_wire.as_str())
        );
        assert!(records.iter().all(|r| r.name == "foo"), "{records:?}");
    }

    /// The bare name is the lookup's first candidate in a directory, ahead of
    /// every `PATHEXT` extension.
    #[test]
    fn pathext_tie_in_one_directory_goes_to_the_bare_name_first() {
        let exe = PathBuf::from("/d/rwv-foo.exe");
        let bare = PathBuf::from("/d/rwv-foo");
        let records = plugin_records(vec![exe, bare.clone()], &windows_default_pathext());
        let winner = records.iter().find(|r| !r.shadowed).expect("a winner");
        assert_eq!(
            winner.path,
            crate::path_spelling::WirePath::of(&bare),
            "{records:?}"
        );
    }

    /// Dispatch walks directories before extensions: an earlier directory's
    /// `.cmd` beats a later directory's `.exe`.
    #[test]
    fn pathext_directory_order_outranks_extension_order() {
        let first_dir_cmd = PathBuf::from("/d1/rwv-foo.cmd");
        let later_dir_exe = PathBuf::from("/d2/rwv-foo.exe");
        let records = plugin_records(
            vec![first_dir_cmd.clone(), later_dir_exe],
            &windows_default_pathext(),
        );
        let winner = records.iter().find(|r| !r.shadowed).expect("a winner");
        assert_eq!(
            winner.path,
            crate::path_spelling::WirePath::of(&first_dir_cmd),
            "{records:?}"
        );
    }

    /// Every listed verb resolves, through the lookup dispatch uses, to the
    /// record listed as its winner — on the host's own naming convention.
    /// Off Windows this reaches directory order and the unstripped extension;
    /// the in-directory `PATHEXT` tie exists only on a Windows run.
    #[test]
    fn discovery_winner_is_what_dispatch_resolves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir1 = tmp.path().join("d1");
        let dir2 = tmp.path().join("d2");
        fs::create_dir_all(&dir1).unwrap();
        fs::create_dir_all(&dir2).unwrap();
        make_executable(&dir1, "rwv-tool");
        make_executable(&dir2, "rwv-tool");
        make_executable(&dir1, "rwv-script.py");
        let mut expected = vec!["script.py", "tool"];
        if cfg!(windows) {
            fs::write(dir2.join("rwv-tie.cmd"), "@echo cmd\r\n").unwrap();
            fs::write(dir2.join("rwv-tie.bat"), "@echo bat\r\n").unwrap();
            expected.insert(1, "tie");
        }
        let path = join_paths(&[dir1.as_path(), dir2.as_path()]);
        let records = discover_plugins(Some(path.as_os_str()));

        let listed: Vec<&str> = records
            .iter()
            .filter(|r| !r.shadowed)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(listed, expected, "{records:?}");

        for r in &records {
            let dispatched = which::which_in(format!("rwv-{}", r.name), Some(&path), tmp.path())
                .unwrap_or_else(|e| panic!("`rwv {}` does not dispatch: {e}; {records:?}", r.name));
            let dispatched = crate::path_spelling::wire_path(&dispatched);
            let winner = r.shadowed_by.as_ref().unwrap_or(&r.path).as_str();
            assert_eq!(
                winner, dispatched,
                "listing and dispatch disagree on `{}`",
                r.name
            );
        }
    }
}
