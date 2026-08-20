//! The `rwv` binary. A shim, deliberately: everything it could hold lives in
//! `repoweave::cli::dispatch` instead.
//!
//! A `[[bin]]` target is a separate crate from the `[lib]`, so anything
//! written here can only reach the library through its `pub` surface. Dispatch
//! mints the consent tokens that authorize destructive ref writes, and minting
//! them from out here would mean publishing a `pub` constructor that every
//! module of the library — `vcs.rs` included — could call. Keeping this file
//! empty of logic is what lets that constructor stay `pub(in crate::cli)`.

fn main() -> std::process::ExitCode {
    repoweave::cli::dispatch::run_and_report()
}
