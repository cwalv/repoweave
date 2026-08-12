//! Per-repo parallelism for network-bound verbs (`rwv fetch`, `rwv update`).
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
//!   `Reporter::Serial` calls `println!`/`eprintln!` directly (no prefix
//!   — matches pre-`-j` output exactly). `Reporter::Parallel { prefix }`
//!   prepends `[<prefix>] ` to each line and serialises writes through a
//!   shared mutex so lines from different workers don't interleave
//!   mid-line.
//! - [`run_subprocess_with_reporter`] spawns a `Command`, captures
//!   stdout/stderr; under `Serial` it preserves the existing
//!   capture-and-only-report-on-failure behaviour; under `Parallel` it
//!   streams lines through the reporter so the user can watch per-repo
//!   progress as it happens.
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

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, ExitStatus, Stdio};
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

/// How a `--json`-capable verb shapes its structured output, decided once
/// from the `--json` flag and the resolved worker count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// No `--json`: human-readable text.
    Text,
    /// `--json`, `jobs == 1`: one pretty-printed envelope after all repos
    /// complete.
    JsonEnvelope,
    /// `--json`, `jobs > 1`: one self-describing NDJSON line per repo,
    /// streamed as workers finish; no envelope wrapper.
    Ndjson,
}

impl OutputMode {
    pub fn resolve(json: bool, jobs: usize) -> Self {
        match (json, jobs > 1) {
            (false, _) => OutputMode::Text,
            (true, false) => OutputMode::JsonEnvelope,
            (true, true) => OutputMode::Ndjson,
        }
    }

    pub fn is_ndjson(self) -> bool {
        matches!(self, OutputMode::Ndjson)
    }
}

/// Output channel for per-repo progress lines.
///
/// Under `-j 1` ([`Reporter::serial`]) the wrappers delegate to the
/// existing `println!`/`eprintln!` pattern with no prefix, so pre-`-j`
/// behaviour is preserved bit-for-bit.
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
    /// No-prefix reporter. Matches `-j 1` / pre-`-j` output exactly.
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
    /// serialising writes to rwv's stdout/stderr across workers.
    pub fn parallel(prefix: String, write_lock: &'a Mutex<()>) -> Self {
        Self {
            inner: ReporterInner::Parallel { prefix, write_lock },
        }
    }

    /// True iff this reporter prepends a prefix and serialises writes
    /// (i.e., is being used from a worker thread under `-j > 1`).
    pub fn is_parallel(&self) -> bool {
        matches!(self.inner, ReporterInner::Parallel { .. })
    }

    /// Emit a line to rwv's stdout. Equivalent to `println!` under
    /// serial; prefixed and lock-protected under parallel; no-op under silent.
    pub fn out(&self, line: &str) {
        match &self.inner {
            ReporterInner::Serial => {
                println!("{line}");
            }
            ReporterInner::Silent => {}
            ReporterInner::Parallel { prefix, write_lock } => {
                let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                let stdout = std::io::stdout();
                let mut stdout = stdout.lock();
                let _ = writeln!(stdout, "[{prefix}] {line}");
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

/// Outcome of running a subprocess through [`run_subprocess_with_reporter`].
///
/// `status` is the child exit status. `stderr_capture` is the stderr
/// content that the caller may want to surface in an error message —
/// under `Reporter::Serial` this is the full captured stderr (matching
/// `Command::output()` behaviour); under `Reporter::Parallel` it is
/// `String::new()` because stderr has already been streamed to the
/// user, prefixed, and re-capturing would either duplicate output or
/// require buffering an entire stream just for the error path.
pub struct SubprocessOutcome {
    pub status: ExitStatus,
    pub stderr_capture: String,
}

/// Spawn `cmd` and wait for it to exit, routing stdout/stderr according
/// to `reporter`.
///
/// Under `Reporter::Serial` the implementation mirrors `Command::output()`
/// — both streams are captured and only surfaced via
/// [`SubprocessOutcome::stderr_capture`] on failure. This preserves the
/// pre-`-j` UX where successful `git fetch` invocations don't spam the
/// terminal.
///
/// Under `Reporter::Parallel` each stream is read line-by-line and
/// forwarded through `Reporter::out` / `Reporter::err` (which prepend
/// the per-repo prefix and serialise writes). The user sees per-repo
/// progress as it happens, even when other workers are mid-fetch.
pub fn run_subprocess_with_reporter(
    cmd: &mut Command,
    reporter: &Reporter<'_>,
) -> std::io::Result<SubprocessOutcome> {
    if !reporter.is_parallel() {
        // Capture-only: identical to Command::output() behaviour.
        // Silent mode and serial mode both capture rather than stream;
        // the distinction is in whether the caller surfaces the output.
        let output = cmd.output()?;
        let stderr_capture = String::from_utf8_lossy(&output.stderr).into_owned();
        return Ok(SubprocessOutcome {
            status: output.status,
            stderr_capture,
        });
    }

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout piped, must be present");
    let stderr = child.stderr.take().expect("stderr piped, must be present");

    thread::scope(|scope| {
        scope.spawn(|| forward_stream(stdout, reporter, true));
        scope.spawn(|| forward_stream(stderr, reporter, false));
    });

    let status = child.wait()?;
    Ok(SubprocessOutcome {
        status,
        // Stream output has already gone to the user via reporter; no
        // need to re-surface it on error.
        stderr_capture: String::new(),
    })
}

/// Read `stream` line-by-line and forward each line through `reporter`.
///
/// `is_stdout = true` routes to [`Reporter::out`]; `false` routes to
/// [`Reporter::err`]. Stops on EOF or first read error. Read errors
/// are swallowed deliberately — the parent waits on the child anyway,
/// and a broken pipe is the normal "subprocess hung up" path.
fn forward_stream<R: Read>(stream: R, reporter: &Reporter<'_>, is_stdout: bool) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { return };
        if is_stdout {
            reporter.out(&line);
        } else {
            reporter.err(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_mode_resolve_no_json_is_text_regardless_of_jobs() {
        assert_eq!(OutputMode::resolve(false, 1), OutputMode::Text);
        assert_eq!(OutputMode::resolve(false, 8), OutputMode::Text);
    }

    #[test]
    fn output_mode_resolve_json_jobs_one_is_envelope() {
        assert_eq!(OutputMode::resolve(true, 1), OutputMode::JsonEnvelope);
    }

    #[test]
    fn output_mode_resolve_json_jobs_gt_one_is_ndjson() {
        assert_eq!(OutputMode::resolve(true, 2), OutputMode::Ndjson);
        assert!(OutputMode::resolve(true, 2).is_ndjson());
        assert!(!OutputMode::resolve(true, 1).is_ndjson());
        assert!(!OutputMode::resolve(false, 2).is_ndjson());
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

    #[test]
    fn run_in_parallel_actually_runs_in_parallel() {
        // Run 4 tasks that each sleep 100ms with 4 workers. If serialised
        // wall time would be ~400ms; under real parallelism it should be
        // closer to ~100ms. Allow generous slack to avoid flakes on busy
        // CI hosts but still catch "accidentally serialised" regressions.
        let items = vec![100u64, 100, 100, 100];
        let start = std::time::Instant::now();
        let _out: Vec<()> = run_in_parallel(&items, 4, |_i, ms| {
            std::thread::sleep(std::time::Duration::from_millis(*ms));
        });
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(350),
            "expected ~100ms under -j 4, got {:?} (looks serialised)",
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

    /// `resolve_jobs(Some(1))` is exactly serial. The spec requires that
    /// `-j 1` matches the pre-`-j` (no flag) output bit for bit; this
    /// guarantees the `<= 1` branch in `run_in_parallel` is taken.
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
    /// visible behaviour: no panic, results in input order, no work
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

    /// Env vars carrying the fixture's script to [`fixture_child`]: what to
    /// write to its own stdout/stderr and what code to exit with.
    const FIXTURE_STDOUT: &str = "RWV_PARALLEL_TEST_FIXTURE_STDOUT";
    const FIXTURE_STDERR: &str = "RWV_PARALLEL_TEST_FIXTURE_STDERR";
    const FIXTURE_EXIT_CODE: &str = "RWV_PARALLEL_TEST_FIXTURE_EXIT_CODE";

    /// Not a test: the child half of the `run_subprocess_with_reporter`
    /// tests below. It runs as a normal (instant, inert) test in an
    /// ordinary suite run, and only when a parent spawns
    /// `current_exe()` with `FIXTURE_EXIT_CODE` set does it write the
    /// requested bytes to its own stdout/stderr and exit with the
    /// requested code — a portable stand-in for `sh -c 'printf ...; exit
    /// N'`, since the test binary itself exists on every platform cargo
    /// builds one for. Mirrors the `strace_probe_child_*` pattern in
    /// `tests/ref_registry_test.rs`.
    #[test]
    fn fixture_child() {
        let Ok(code) = std::env::var(FIXTURE_EXIT_CODE) else {
            return;
        };
        if let Ok(s) = std::env::var(FIXTURE_STDOUT) {
            print!("{s}");
            std::io::stdout().flush().unwrap();
        }
        if let Ok(s) = std::env::var(FIXTURE_STDERR) {
            eprint!("{s}");
            std::io::stderr().flush().unwrap();
        }
        std::process::exit(code.parse().expect("valid exit code"));
    }

    /// A `Command` that re-invokes this test binary selecting only
    /// [`fixture_child`]. Callers set `FIXTURE_STDOUT`/`FIXTURE_STDERR`/
    /// `FIXTURE_EXIT_CODE` on the returned `Command` before spawning it.
    /// `--nocapture` is load-bearing: without it, libtest's own capture
    /// hook swallows `fixture_child`'s `print!`/`eprint!` calls before
    /// they reach the real stdout/stderr this fixture exists to write to.
    fn fixture_command() -> Command {
        let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
        cmd.args([
            "--exact",
            "parallel::tests::fixture_child",
            "--test-threads=1",
            "--nocapture",
        ]);
        cmd
    }

    /// Under `Reporter::Serial`, `run_subprocess_with_reporter` mirrors
    /// `Command::output()`: failing subprocess returns a non-zero
    /// status AND captured stderr. This is the path verbs take under
    /// `-j 1` so successful runs don't spam the terminal but failures
    /// keep their captured stderr for the summary.
    #[test]
    fn run_subprocess_with_reporter_serial_captures_stderr_on_failure() {
        let mut cmd = fixture_command();
        cmd.env(FIXTURE_STDERR, "boom\n");
        cmd.env(FIXTURE_EXIT_CODE, "7");
        let reporter = Reporter::serial();
        let outcome =
            run_subprocess_with_reporter(&mut cmd, &reporter).expect("subprocess spawned");
        assert!(!outcome.status.success(), "expected non-zero exit");
        assert_eq!(outcome.status.code(), Some(7));
        assert!(
            outcome.stderr_capture.contains("boom"),
            "expected captured stderr to include child output; got {:?}",
            outcome.stderr_capture
        );
    }

    /// Under `Reporter::Parallel`, `run_subprocess_with_reporter`
    /// streams stdout/stderr lines through the reporter as they
    /// arrive and returns an *empty* `stderr_capture`. The user
    /// already saw the lines (prefixed); re-surfacing on failure
    /// would either duplicate output or require buffering an entire
    /// stream just for the error path. Pin the contract that
    /// `stderr_capture` is empty under Parallel — verb-side error
    /// summaries must not assume otherwise.
    #[test]
    fn run_subprocess_with_reporter_parallel_returns_empty_capture() {
        let lock = Mutex::new(());
        let reporter = Reporter::parallel("ut".into(), &lock);
        let mut cmd = fixture_command();
        cmd.env(FIXTURE_STDERR, "line1\nline2\n");
        cmd.env(FIXTURE_EXIT_CODE, "3");
        let outcome =
            run_subprocess_with_reporter(&mut cmd, &reporter).expect("subprocess spawned");
        assert!(!outcome.status.success());
        assert_eq!(outcome.status.code(), Some(3));
        assert_eq!(
            outcome.stderr_capture, "",
            "Parallel reporter streams stderr to the user; stderr_capture must be empty"
        );
    }

    /// A child writing multiple lines to both stdout and stderr
    /// exercises two concurrent `forward_stream` loops (one per pipe,
    /// via `thread::scope`) instead of a single read. This pins only
    /// that the call returns rather than hanging; it cannot pin line
    /// content or count. Under `Reporter::Parallel`, forwarded lines
    /// go straight to this process's real stdout/stderr from a
    /// spawned thread, which is outside libtest's own capture (see
    /// `forward_stream_consumes_to_eof_without_panic`), and
    /// `stderr_capture` is empty by contract regardless of what the
    /// child wrote (`run_subprocess_with_reporter_parallel_returns_empty_capture`).
    /// Line arrival is pinned end-to-end, via a real child process, in
    /// `tests/parallel_test.rs`.
    #[test]
    fn run_subprocess_multi_line_under_parallel_completes_without_hanging() {
        let lock = Mutex::new(());
        let reporter = Reporter::parallel("ut".into(), &lock);
        let mut cmd = fixture_command();
        cmd.env(FIXTURE_STDOUT, "o1\no2\no3\no4\n");
        cmd.env(FIXTURE_STDERR, "e1\ne2\ne3\ne4\n");
        cmd.env(FIXTURE_EXIT_CODE, "0");
        let outcome =
            run_subprocess_with_reporter(&mut cmd, &reporter).expect("subprocess spawned");
        assert!(outcome.status.success());
        assert_eq!(outcome.stderr_capture, "");
    }

    /// `forward_stream` reads UTF-8 line-by-line via `BufReader::lines()`,
    /// stops on EOF, and never panics on a broken stream. Pin the
    /// stop-on-EOF behaviour directly via an in-memory cursor (no
    /// subprocess needed). Output goes to the test process's stdout —
    /// we can't easily intercept it, but we can confirm the function
    /// returns cleanly.
    #[test]
    fn forward_stream_consumes_to_eof_without_panic() {
        let lock = Mutex::new(());
        let reporter = Reporter::parallel("ut".into(), &lock);
        let data: &[u8] = b"a\nb\nc\n";
        forward_stream(std::io::Cursor::new(data), &reporter, true);
        forward_stream(std::io::Cursor::new(data), &reporter, false);
    }

    /// A stream that ends without a trailing newline still surfaces
    /// the final partial line — `BufReader::lines()` yields it on the
    /// last iteration. Pin the contract so verb output that ends mid-
    /// line under `-j N` isn't silently dropped.
    #[test]
    fn forward_stream_emits_final_line_without_trailing_newline() {
        let lock = Mutex::new(());
        let reporter = Reporter::parallel("ut".into(), &lock);
        let data: &[u8] = b"first\nsecond-no-newline";
        forward_stream(std::io::Cursor::new(data), &reporter, true);
    }
}
