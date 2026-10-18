//! The `rwv` binary. A shim, deliberately: everything it could hold lives in
//! `repoweave::cli::dispatch` instead.
//!
//! A `[[bin]]` target is a separate crate from the `[lib]`, so anything
//! written here can only reach the library through its `pub` surface. Dispatch
//! mints the consent tokens that authorize destructive ref writes, and minting
//! them from out here would mean publishing a `pub` constructor that every
//! module of the library — `vcs.rs` included — could call. Keeping this file
//! empty of logic is what lets that constructor stay `pub(in crate::cli)`.

/// One stack size on every platform: Windows gives the main thread 1 MiB, less
/// than an unoptimized build needs to parse its arguments.
const COMMAND_STACK_BYTES: usize = 8 << 20;

fn main() -> std::process::ExitCode {
    std::thread::Builder::new()
        .stack_size(COMMAND_STACK_BYTES)
        .spawn(repoweave::cli::dispatch::run_and_report)
        .expect("the OS refused the thread rwv runs its command on")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
