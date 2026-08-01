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

/// The `rwv` version: `build.rs`'s `git describe` output when built inside a
/// git checkout (e.g. `0.16.0-3-ge5bfa9f`), else `Cargo.toml`'s version.
/// The one fact `--version` and the plugin envelope's `RWV_VERSION` both read.
pub fn rwv_version() -> &'static str {
    option_env!("RWV_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

pub mod activate;
pub mod add_remove;
pub mod check;
pub mod cli;
pub mod durable_file;
pub mod explain;
pub mod fetch;
pub mod git;
pub mod init;
pub mod integration;
pub mod integration_runner;
pub mod integrations;
pub mod lock;
pub mod manifest;
pub mod op_state;
pub mod parallel;
pub mod plugins;
pub mod prime;
pub mod push;
pub mod registry;
mod schema_url;
pub mod selector;
pub mod setup;
pub mod status;
pub mod sync;
pub mod update;
pub mod vcs;
pub mod workspace;
pub mod workweave;
pub mod workweave_index;
