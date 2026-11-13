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
//!   convention.** [`vcs`] admits a ref write only against a witness, proof
//!   or consent token that cannot be forged from a string.
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

/// A temporary directory under the resolved spelling of the temp root; every
/// inline `#[cfg(test)]` module takes its fixture roots from here.
///
/// The counterpart to `tempdir` in `tests/common/mod.rs`, which the library
/// crate cannot see. rwv records and reports resolved locations, and `$TMPDIR`
/// may sit behind a symlink — on macOS `/var` is one to `/private/var` — so a
/// path a test joins onto the raw root is a second spelling of the location
/// rwv names. Resolved once here, every path derived from the root is the
/// spelling rwv reports.
#[cfg(test)]
pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let raw = std::env::temp_dir();
        path_spelling::resolve(&raw)
            .unwrap_or_else(|e| panic!("temp dir {} does not resolve: {e}", raw.display()))
    });
    tempfile::TempDir::new_in(root)
}

/// Write `contents` to `path` as a program a test will run, mode 0o755.
///
/// The counterpart to `write_executable` in `tests/common/mod.rs`. Exec fails
/// with ETXTBSY while any process holds the file open for writing, and a
/// write handle this process held is inherited by every child another test
/// thread forks until that child execs; a rename does not help, since the
/// inherited handle is to the file. So a child process writes it, and this
/// process never holds a write handle to it.
#[cfg(test)]
pub(crate) fn write_executable(path: &std::path::Path, contents: impl AsRef<[u8]>) {
    #[cfg(unix)]
    {
        use std::io::Write;
        let mut writer = std::process::Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!("writing {} through sh failed to start: {e}", path.display())
            });
        writer
            .stdin
            .take()
            .expect("the writer's stdin is piped")
            .write_all(contents.as_ref())
            .unwrap_or_else(|e| panic!("feeding {} failed: {e}", path.display()));
        let status = writer.wait().expect("the writer is waited on");
        assert!(
            status.success(),
            "writing {} exited {status}",
            path.display()
        );
    }
    #[cfg(not(unix))]
    std::fs::write(path, contents)
        .unwrap_or_else(|e| panic!("writing {} failed: {e}", path.display()));
}

#[macro_use]
mod stdout;
pub mod activate;
pub mod add_remove;
pub mod alignment;
pub mod backup;
pub mod branch;
pub mod branch_ref;
pub mod check;
pub mod cherry_pick;
pub mod cli;
pub mod clone;
pub mod commit_guard;
pub mod declarations;
pub mod deletion;
pub mod diagnosis;
pub mod durable_file;
pub mod entry_report;
pub mod exit_status;
pub mod explain;
pub mod feature_sync;
pub mod fetch;
pub mod fetch_claim;
pub mod git;
pub mod health_floor;
pub mod init;
pub mod integration;
pub mod integration_runner;
pub mod integrations;
pub mod land;
pub mod landing_claim;
pub(crate) mod lineage;
pub mod lock;
pub mod lock_commit;
pub mod lock_driver;
pub mod lock_merge;
pub mod log;
pub mod manifest;
pub mod manifest_commit;
pub(crate) mod manifest_conflict;
pub mod member_branch;
pub mod member_pin;
pub mod member_selection;
pub mod member_settings;
pub mod member_url;
pub mod merge_upstream;
pub(crate) mod names;
pub mod naming;
pub mod op_state;
pub mod owned_state;
pub mod parallel;
pub mod path_spelling;
pub mod pins;
pub mod placement;
pub mod plugins;
pub mod prime;
pub mod project_merge;
pub mod pull;
pub mod push;
pub(crate) mod push_branch;
pub mod records;
pub mod refusal;
pub mod registry;
pub(crate) mod remap;
pub mod repin;
pub mod root_write;
mod schema_url;
pub mod selector;
pub mod set_history;
pub mod settle;
pub mod setup;
pub mod share;
pub(crate) mod shared_members;
pub mod skipped;
pub(crate) mod stack;
pub mod state_file;
pub mod status;
pub mod switch;
pub mod symlink;
pub mod sync;
pub(crate) mod test_pause;
pub(crate) mod tool_program;
pub mod unmerged;
pub mod update;
pub mod update_remote;
pub mod url_remote;
pub mod vcs;
mod wire_token;
pub mod workspace;
pub mod workweave;
pub mod workweave_index;
