//! repoweave (`rwv`) — a workspace manager for a tree of independently
//! versioned repositories.
//!
//! A *weave* is a directory holding many repository clones plus one or more
//! *projects*. A project is a manifest (`rwv.toml`) and a lock (`rwv.lock`),
//! both committed to a project repo, declaring which repositories it uses and
//! at which revisions. The verbs converge disk to that committed pair, move it
//! forward, or fan work out into parallel checkouts (*workweaves*).
//!
//! Three properties shape the whole crate:
//!
//! - **Single-shot.** Every invocation resolves its context from the
//!   filesystem and exits. No daemon, no IPC, no state that outlives the
//!   process.
//! - **The filesystem is the only persistence layer.** Durable state is a
//!   handful of marker and record files ([`workspace`], [`op_state`],
//!   [`workweave_index`]) plus the version control system's own refs.
//! - **Destructive operations are gated by the type system, not by
//!   convention.** [`vcs`] admits a ref write only against a witness,
//!   receipt, warrant or consent token that cannot be forged from a string.
//!
//! `ARCHITECTURE.md` at the repo root is the module map, dataflow and seam
//! description; `docs/explanation/joints/` holds the normative contracts.

/// The version `Cargo.toml` declares: the only version `--version`, the plugin
/// envelope's `RWV_VERSION` and the health floor carry.
pub fn rwv_version() -> &'static semver::Version {
    static VERSION: std::sync::LazyLock<semver::Version> = std::sync::LazyLock::new(|| {
        env!("CARGO_PKG_VERSION")
            .parse()
            .expect("cargo builds only a package whose version is semver")
    });
    &VERSION
}

/// The `git describe` of the checkout this binary was built in; absent for a
/// build outside one. Build information to print, never to compare or record.
pub fn rwv_git_describe() -> Option<&'static str> {
    option_env!("RWV_GIT_DESCRIBE")
}

/// The Cargo version, followed in parentheses by the build's `git describe`
/// when it has one.
pub fn rwv_version_with_build() -> &'static str {
    static TEXT: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| match rwv_git_describe() {
            Some(describe) => format!("{} ({describe})", rwv_version()),
            None => rwv_version().to_string(),
        });
    &TEXT
}

/// Announce a test that is about to return without measuring anything.
///
/// The inline `#[cfg(test)]` modules' counterpart to `report_skip` in
/// `tests/common/mod.rs`, which they cannot name: that module is compiled into
/// each test TARGET and the library crate cannot see it. Same bytes for the
/// same reason — libtest intercepts the print macros and discards what a
/// PASSING test wrote, so a skip announced with `eprintln!` reaches nobody, and
/// a reader who knew to pass `--nocapture` already suspected the skip.
///
/// One `write_all` of a whole line: tests run concurrently, and a notice torn
/// across two threads is worse than none.
#[cfg(test)]
pub(crate) fn report_skip(reason: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(format!("SKIP: {reason}\n").as_bytes());
}

#[macro_use]
mod stdout;
pub mod activate;
pub mod add_remove;
pub mod check;
pub mod cli;
pub mod durable_file;
pub mod explain;
pub mod fetch;
pub mod git;
pub mod health_floor;
pub mod init;
pub mod integration;
pub mod integration_runner;
pub mod integrations;
pub mod lock;
pub mod manifest;
pub mod naming;
pub mod op_state;
pub mod owned_state;
pub mod parallel;
pub mod path_spelling;
pub mod placement;
pub mod plugins;
pub mod prime;
pub mod push;
pub mod refusal;
pub mod registry;
pub mod role;
pub mod root_write;
mod schema_url;
pub mod selector;
pub mod settle;
pub mod setup;
pub mod share;
pub mod skipped;
pub mod state_file;
pub mod status;
pub mod symlink;
pub mod sync;
pub(crate) mod tool_program;
pub mod update;
pub mod vcs;
mod wire_token;
pub mod workspace;
pub mod workweave;
pub mod workweave_index;
