//! Per-repo parallelism for network-bound verbs (`rwv fetch`, `rwv update`, `rwv update --remote`, `rwv clone`).
//!
//! This module owns the small set of pieces those verbs need to fan out
//! their per-repo loop across worker threads:
//!
//! - [`resolve_jobs`] turns a `Option<usize>` flag into a concrete worker
//!   count (`min(available_parallelism, 8)` by default; `0` = unlimited
//!   debug-only path).
//! - [`run_in_parallel`] takes a list of work items plus a closure and runs
//!   the closure over each item with a bounded worker pool, gathering
//!   results in input order. Uses `std::thread::scope` so closures can
//!   borrow from the caller's stack (no `Arc<Mutex<_>>` over shared
//!   read-only data needed).
//! - [`Reporter`] abstracts how per-repo progress lines reach the user.
//!   `Reporter::Serial` calls `println!`/`eprintln!` directly, with no
//!   prefix. `Reporter::Parallel { prefix }` prepends `[<prefix>] ` to each
//!   line and serializes writes through a shared mutex so lines from
//!   different workers don't interleave mid-line.
//!
//! Design notes:
//!
//! - The worker pool is built on `std::thread::scope` rather than rayon /
//!   tokio. The crate is otherwise std-only; adding a heavy dep for two
//!   verbs would be disproportionate. Scoped threads let workers borrow
//!   slices of the manifest, the reporter mutex, etc. directly.
//! - Job dispatch is a shared `Mutex<usize>` cursor over the input
//!   slice. Lock contention is at job-fetch boundary only (microseconds);
//!   the actual per-repo work (network git ops) dwarfs it.
//! - Output ordering: in `Parallel` mode the order of `[prefix] line`
//!   writes reflects real-time interleaving of worker threads (matches
//!   `make -j` / `ninja` conventions). Aggregated errors are returned in
//!   input order so the failure report shape doesn't depend on
//!   scheduling.

use std::io::Write;
use std::sync::Mutex;
use std::thread;

/// Hard cap on the auto-resolved worker count. Prevents pathological cases
/// (e.g., a 64-core machine hammering a single forge into rate-limit
/// territory) without requiring per-invocation tuning. Users can override
/// with `-j N` if they need more.
const DEFAULT_JOBS_CAP: usize = 8;

/// Resolve a `-j` flag value to a concrete worker count.
///
/// - `None`: auto — `min(available_parallelism, DEFAULT_JOBS_CAP)`. When
///   `available_parallelism` is unavailable for any reason, falls back
///   to 1 (serial), the safe default.
/// - `Some(0)`: unlimited (debug-only; one worker per item). Undocumented.
/// - `Some(n)`: exactly `n` workers.
pub fn resolve_jobs(jobs: Option<usize>) -> usize {
    match jobs {
        None => match thread::available_parallelism() {
            Ok(n) => n.get().min(DEFAULT_JOBS_CAP),
            Err(_) => 1,
        },
        Some(0) => usize::MAX, // saturates to "one worker per item"
        Some(n) => n,
    }
}

/// Whether a `--json`-capable verb prints text or its JSON envelope. The
/// worker count has no say: `-j` changes how the work runs, never what is
/// printed about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Text,
    /// One pretty-printed envelope after the run completes.
    Json,
}

impl OutputMode {
    pub fn from_flag(json: bool) -> Self {
        if json {
            OutputMode::Json
        } else {
            OutputMode::Text
        }
    }

    pub fn is_json(self) -> bool {
        self == OutputMode::Json
    }
}

/// Output channel for per-repo progress lines.
///
/// Under `-j 1` ([`Reporter::serial`]) the wrappers write through
/// `println!`/`eprintln!` with no prefix: each line reaches the stream
/// exactly as the caller wrote it.
///
/// Under `-j > 1` ([`Reporter::parallel`]) each emit prepends
/// `[<prefix>] ` and takes a shared lock so concurrent workers don't
/// produce torn lines. The lock guards the rwv process's stdout and
/// stderr handles together — flushing one side then the other is fine,
/// but two threads must not be in the middle of writing the same stream
/// at once.
pub struct Reporter<'a> {
    inner: ReporterInner<'a>,
}

enum ReporterInner<'a> {
    Serial,
    Silent,
    Parallel {
        prefix: String,
        write_lock: &'a Mutex<()>,
    },
}

impl<'a> Reporter<'a> {
    /// No-prefix reporter: each line is written exactly as given.
    pub fn serial() -> Self {
        Self {
            inner: ReporterInner::Serial,
        }
    }

    /// Silent reporter. Discards all output — used under `--json` where
    /// structured records replace text-mode chatter on stdout.
    pub fn silent() -> Self {
        Self {
            inner: ReporterInner::Silent,
        }
    }

    /// Prefixed reporter. `prefix` is the per-line tag (typically the
    /// manifest's `repo_path`); `write_lock` is the shared mutex
    /// serializing writes to rwv's stdout/stderr across workers.
    pub fn parallel(prefix: String, write_lock: &'a Mutex<()>) -> Self {
        Self {
            inner: ReporterInner::Parallel { prefix, write_lock },
        }
    }

    /// True iff this reporter prepends a prefix and serializes writes
    /// (i.e., is being used from a worker thread under `-j > 1`).
    pub fn is_parallel(&self) -> bool {
        matches!(self.inner, ReporterInner::Parallel { .. })
    }

    /// Emit a line to rwv's stdout. Equivalent to `println!` under
    /// serial; prefixed and lock-protected under parallel; no-op under silent.
    pub fn out(&self, line: &str) {
        match &self.inner {
            ReporterInner::Serial => {
                outln!("{line}");
            }
            ReporterInner::Silent => {}
            ReporterInner::Parallel { prefix, write_lock } => {
                let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                let _ = crate::stdout::write(format_args!("[{prefix}] {line}"), true);
            }
        }
    }

    /// Emit a line to rwv's stderr. Equivalent to `eprintln!` under
    /// serial; prefixed and lock-protected under parallel; no-op under silent.
    pub fn err(&self, line: &str) {
        match &self.inner {
            ReporterInner::Serial => {
                eprintln!("{line}");
            }
            ReporterInner::Silent => {}
            ReporterInner::Parallel { prefix, write_lock } => {
                let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                let stderr = std::io::stderr();
                let mut stderr = stderr.lock();
                let _ = writeln!(stderr, "[{prefix}] {line}");
            }
        }
    }
}

/// Run a closure over each item in `items` with up to `jobs` worker
/// threads, returning results in input order.
///
/// `jobs == 1`: runs serially on the caller thread; no spawning at all.
/// `jobs > 1`: spawns `min(jobs, items.len())` workers via
/// `std::thread::scope`. Items are dispatched off a shared cursor, so a
/// slow worker doesn't block the pool — fast workers pick up the next
/// available index.
///
/// The closure must be `Send + Sync` (it runs on multiple threads), the
/// item type must be `Sync` (workers share `&items[i]`), and the result
/// type must be `Send`. The result vector preserves input order, which
/// is what the existing error-aggregation in `update.rs` / `fetch.rs`
/// expects.
pub fn run_in_parallel<T, R, F>(items: &[T], jobs: usize, work: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, &T) -> R + Send + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    if jobs <= 1 {
        return items
            .iter()
            .enumerate()
            .map(|(i, item)| work(i, item))
            .collect();
    }

    let n = items.len();
    let worker_count = jobs.min(n);
    // Pre-allocate the result vector. Workers write into their own slot
    // by index; each slot is written by exactly one worker, so a
    // Mutex<Vec<R>> would be unnecessary contention. Using a Mutex over
    // each slot would be wasteful too; instead we hand each worker the
    // raw indices through a shared cursor and have them stash results
    // in a Mutex<Vec<Option<R>>>. We then drain into a final Vec<R> in
    // input order.
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..n).map(|_| None).collect());
    let cursor: Mutex<usize> = Mutex::new(0);

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let work = &work;
            let results = &results;
            let cursor = &cursor;
            scope.spawn(move || loop {
                let idx = {
                    let mut c = cursor.lock().unwrap_or_else(|e| e.into_inner());
                    if *c >= n {
                        return;
                    }
                    let i = *c;
                    *c += 1;
                    i
                };
                let r = work(idx, &items[idx]);
                let mut guard = results.lock().unwrap_or_else(|e| e.into_inner());
                guard[idx] = Some(r);
            });
        }
    });

    let mut guard = results.lock().unwrap_or_else(|e| e.into_inner());
    guard.iter_mut().map(|slot| slot.take().unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_mode_follows_the_flag_alone() {
        assert_eq!(OutputMode::from_flag(false), OutputMode::Text);
        assert_eq!(OutputMode::from_flag(true), OutputMode::Json);
    }

    #[test]
    fn resolve_jobs_some_n_returns_n() {
        assert_eq!(resolve_jobs(Some(1)), 1);
        assert_eq!(resolve_jobs(Some(4)), 4);
        assert_eq!(resolve_jobs(Some(16)), 16);
    }

    #[test]
    fn resolve_jobs_zero_is_saturating() {
        // -j 0 is the debug "unlimited" path; we encode it as usize::MAX
        // and trust run_in_parallel to clamp to items.len().
        assert_eq!(resolve_jobs(Some(0)), usize::MAX);
    }

    #[test]
    fn resolve_jobs_default_is_capped() {
        // Whatever the host has, the auto-resolved value never exceeds
        // the documented cap.
        let resolved = resolve_jobs(None);
        assert!(resolved >= 1, "resolved={resolved}");
        assert!(
            resolved <= DEFAULT_JOBS_CAP,
            "resolved={resolved} exceeds cap={DEFAULT_JOBS_CAP}"
        );
    }

    #[test]
    fn run_in_parallel_serial_passthrough() {
        let items = vec![1u32, 2, 3, 4, 5];
        let out: Vec<u32> = run_in_parallel(&items, 1, |_i, x| *x * 2);
        assert_eq!(out, vec![2, 4, 6, 8, 10]);
    }

    #[test]
    fn run_in_parallel_preserves_input_order_under_concurrency() {
        // Even though workers finish in arbitrary order, results map back
        // to their input index. Add a small jittery sleep to make
        // re-ordering likely under interleaving.
        let items: Vec<u32> = (0..32).collect();
        let out: Vec<u32> = run_in_parallel(&items, 8, |_i, x| {
            // Reverse-jitter: larger inputs sleep less, so they finish
            // first. If results came back in completion order rather
            // than input order, the vec would be reversed.
            let sleep_ms = 32 - *x;
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms.into()));
            *x * 2
        });
        let expected: Vec<u32> = items.iter().map(|x| *x * 2).collect();
        assert_eq!(out, expected);
    }

    /// Silent serialization is invisible to `tests/process/parallel_test.rs`: with
    /// the pool clamped to one worker every `-j` acceptance test there still
    /// passes, because the prefix, the lock and the aggregation depend on
    /// `jobs > 1`, not on the workers existing. Wall time is the only
    /// observable that separates them.
    #[test]
    fn run_in_parallel_actually_runs_in_parallel() {
        // Run 4 tasks that each sleep 100ms with 4 workers. If serialized
        // wall time would be ~400ms; under real parallelism it should be
        // closer to ~100ms. Allow generous slack to avoid flakes on busy
        // CI hosts but still catch "accidentally serialized" regressions.
        let items = vec![100u64, 100, 100, 100];
        let start = std::time::Instant::now();
        let _out: Vec<()> = run_in_parallel(&items, 4, |_i, ms| {
            std::thread::sleep(std::time::Duration::from_millis(*ms));
        });
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(350),
            "expected ~100ms under -j 4, got {:?} (looks serialized)",
            elapsed
        );
    }

    #[test]
    fn run_in_parallel_empty_input_is_noop() {
        let items: Vec<u32> = Vec::new();
        let out: Vec<u32> = run_in_parallel(&items, 4, |_i, x| *x);
        assert!(out.is_empty());
    }

    #[test]
    fn reporter_serial_is_not_parallel() {
        let r = Reporter::serial();
        assert!(!r.is_parallel());
    }

    #[test]
    fn reporter_parallel_is_parallel() {
        let lock = Mutex::new(());
        let r = Reporter::parallel("test".into(), &lock);
        assert!(r.is_parallel());
    }

    // ---- additional thin-spot coverage ----------------------------------

    /// `resolve_jobs(Some(1))` is exactly serial: `-j 1` output is the
    /// unprefixed form, which needs the `<= 1` branch in `run_in_parallel`
    /// to be taken.
    #[test]
    fn resolve_jobs_one_is_serial() {
        // resolve_jobs is a pure mapping; pin the value `1` so callers
        // can rely on it.
        assert_eq!(resolve_jobs(Some(1)), 1);
        // The downstream effect: run_in_parallel runs on the caller
        // thread (no thread spawn).
        let items = vec![1u32, 2, 3];
        let main_thread_id = std::thread::current().id();
        let out: Vec<_> = run_in_parallel(&items, 1, |_i, _x| std::thread::current().id());
        for tid in &out {
            assert_eq!(
                *tid, main_thread_id,
                "serial path must not spawn — every closure runs on the caller's thread"
            );
        }
    }

    /// `-j 0` resolves to `usize::MAX` (the debug "unlimited" path) and
    /// `run_in_parallel` clamps the spawn count to `items.len()`. The
    /// visible behavior: no panic, results in input order, no work
    /// dropped. Without clamping, this would attempt to spawn
    /// `usize::MAX` worker threads and either OOM or panic.
    #[test]
    fn run_in_parallel_clamps_jobs_to_item_count() {
        let jobs = resolve_jobs(Some(0));
        assert_eq!(jobs, usize::MAX);
        let items: Vec<u32> = (0..4).collect();
        let out: Vec<u32> = run_in_parallel(&items, jobs, |_i, x| *x * 10);
        assert_eq!(out, vec![0, 10, 20, 30]);
    }

    /// Asking for more workers than items spawns exactly `items.len()`
    /// workers — pool size never exceeds work. Pin this directly: 100
    /// jobs across 3 items must finish without panicking and produce
    /// 3 results in order.
    #[test]
    fn run_in_parallel_jobs_greater_than_items() {
        let items: Vec<u32> = vec![1, 2, 3];
        let out: Vec<u32> = run_in_parallel(&items, 100, |_i, x| *x + 1);
        assert_eq!(out, vec![2, 3, 4]);
    }

    /// Single-item with multiple jobs is a parallel-mode call but only
    /// one work unit. We assert correctness, not single-threadedness,
    /// since the pool is clamped to `min(jobs, items.len())`.
    #[test]
    fn run_in_parallel_single_item_multi_jobs() {
        let items = vec![42u32];
        let out: Vec<u32> = run_in_parallel(&items, 8, |_i, x| *x * 2);
        assert_eq!(out, vec![84]);
    }

    /// The reporter prefix is pasted in verbatim — characters that
    /// look like markup (`[`, `]`, ANSI escapes, even quotes) must
    /// pass through. Repo paths can theoretically contain these (no
    /// VCS forbids them in branch/repo strings); pin the no-escaping
    /// contract.
    #[test]
    fn reporter_parallel_prefix_with_brackets_is_passthrough() {
        let lock = Mutex::new(());
        // Construction must succeed regardless of the prefix string;
        // there's no validation.
        let _r = Reporter::parallel("github/x/y".into(), &lock);
        let _r = Reporter::parallel("with [bracket]".into(), &lock);
        let _r = Reporter::parallel("with \"quote\"".into(), &lock);
        let _r = Reporter::parallel(String::new(), &lock);
    }
}
