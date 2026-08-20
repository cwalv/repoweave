//! `tool_only_bin` hands every caller one persistent shim directory, created
//! on first use: stat the link, create it when absent. Two tests reaching
//! that first use together can both see the link absent, and the later
//! create then fails with `AlreadyExists` although the winner's identical
//! link is already in place — losing that race must read as the success it
//! is. The window only opens where `target/tmp` starts empty (the pre-push
//! hook's throwaway worktree, a CI runner); a warm weave target carries the
//! link over from earlier runs, which is why the loss shows up as a one-test
//! flake in exactly those cold postures and nowhere else.

use std::sync::Barrier;

mod common;

/// Barrier-paired first callers on a removed link, driven through the real
/// `tool_only_bin`, both of which must come back usable. Reddens when the
/// losing caller panics on `AlreadyExists` instead of reusing the winner's
/// link — that red arrives through the join's `resume_unwind`, not through
/// the equality assertion, whose two sides are a pure function of the tool
/// name and cannot differ.
///
/// Whether the race was exercised at all is measured, not assumed: each
/// caller records a wall-clock window around its call, and a round counts
/// only when the two windows overlap. On a host that serializes the pair —
/// a single-core runner schedules one thread to completion — zero rounds
/// overlap, and the run announces itself vacuous instead of passing as
/// evidence.
#[test]
fn concurrent_first_callers_share_the_link() {
    let link = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("only-bin-git")
        .join(if cfg!(windows) { "git.exe" } else { "git" });
    let barrier = Barrier::new(2);
    let mut overlapped = 0u32;
    for _ in 0..64 {
        if link.symlink_metadata().is_ok() {
            repoweave::symlink::remove(&link).expect("the link is removable between rounds");
        }
        let base = std::time::Instant::now();
        let call = |base: std::time::Instant| {
            barrier.wait();
            let start = base.elapsed().as_nanos();
            let dir = common::tool_only_bin("git");
            (dir, start, base.elapsed().as_nanos())
        };
        let ((first, a0, a1), (second, b0, b1)) = std::thread::scope(|s| {
            let first = s.spawn(|| call(base));
            let second = s.spawn(|| call(base));
            (
                first
                    .join()
                    .unwrap_or_else(|p| std::panic::resume_unwind(p)),
                second
                    .join()
                    .unwrap_or_else(|p| std::panic::resume_unwind(p)),
            )
        });
        if a0 < b1 && b0 < a1 {
            overlapped += 1;
        }
        assert_eq!(first, second, "both callers must share one shim directory");
        assert!(
            std::fs::metadata(&link).is_ok(),
            "the shared link must resolve after both calls"
        );
    }
    if overlapped == 0 {
        common::report_skip(
            "tool_only_bin race: no round overlapped its two callers on this \
             host, so the first-use race was never exercised",
        );
    }
}
