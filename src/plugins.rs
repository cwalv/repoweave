//! External-subcommand dispatch: `rwv <verb>` where `<verb>` is not a core
//! verb resolves to `rwv-<verb>` on `$PATH` and execs it, exit/signal
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
//!   status code; signal death maps to the conventional `128 + N` exit and
//!   is reported on stderr as `rwv-<verb> terminated by signal N`.
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
//! The lookup goes through the `which` crate (its `which::which()`), which
//! encapsulates the OS executable-discovery surface. This deliberately
//! avoids an explicit `std::env::var("PATH")` read in this crate's source:
//! PATH is not an addressing input and the env-input inventory rightly
//! forbids ambient env reads for addressing.
//!
//! # Env envelope
//!
//! This module builds the spawn command through a single seam
//! ([`build_command`]) that a sibling change will extend to inject the
//! `RWV_*` context envelope. This module sets no env vars of its own.

use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;

/// Build the `Command` that will spawn `rwv-<verb>` for the given args.
///
/// Single seam: a sibling change injects the `RWV_*` context envelope by
/// extending this function. Callers must not construct the child command
/// inline.
///
/// The child inherits stdin, stdout, and stderr — [`std::process::Command`]
/// does that by default when none of `stdin`/`stdout`/`stderr` are set.
/// This preserves the plugin's terminal control and its own I/O contract.
pub fn build_command(binary: &std::path::Path, args: &[OsString]) -> Command {
    let mut cmd = Command::new(binary);
    cmd.args(args);
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
/// `args`, propagate its exit status. Never returns on success — exits the
/// process with the child's code. Returns an error for the two rwv-side
/// failure modes documented on the module.
///
/// Signal death: mirrored to `128 + N` and reported on stderr. Exit
/// otherwise verbatim.
pub fn dispatch_external(
    verb: &str,
    args: &[OsString],
) -> anyhow::Result<std::convert::Infallible> {
    let binary = find_plugin(verb).ok_or_else(|| unknown_verb_error(verb))?;

    let mut cmd = build_command(&binary, args);
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

    // No exit code → the child was terminated by a signal. Report and
    // mirror the conventional 128 + N mapping so downstream consumers
    // (shells, CI runners) see the standard indication.
    if let Some(sig) = status.signal() {
        eprintln!("rwv-{verb} terminated by signal {sig}");
        std::process::exit(128 + sig);
    }

    // Neither an exit code nor a signal — should be impossible on Unix.
    // Emit a defensive 1 rather than looping forever.
    eprintln!("rwv-{verb} exited abnormally with no status");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_sets_program_and_args() {
        let binary = std::path::Path::new("/tmp/rwv-example");
        let args: Vec<OsString> = vec!["--flag".into(), "value".into(), "--".into(), "-x".into()];
        let cmd = build_command(binary, &args);
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
}
