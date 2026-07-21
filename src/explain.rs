//! `rwv explain <cmd>` — per-verb agent-oriented reflection.
//!
//! Returns a markdown bundle for the requested verb (purpose, invocation,
//! output description with JSON Schema for `--json`-capable verbs).
//!
//! Implementation: build-time generator (`cargo run --bin generate-explain`)
//! assembles markdown from hand-written templates + schemars-derived JSON
//! schemas, writing artifacts to `docs/reference/explain/*.md` and
//! `docs/reference/schemas/*.json`. This module embeds those artifacts via
//! `include_str!()` and dispatches with a trivial match.

// Generated artifacts. The `generate-explain` binary writes these from
// templates + Rust types; CI fails if they drift.
const INDEX_EXPLAIN: &str = include_str!("../docs/reference/explain/index.md");
const STATUS_EXPLAIN: &str = include_str!("../docs/reference/explain/status.md");
const DOCTOR_EXPLAIN: &str = include_str!("../docs/reference/explain/doctor.md");
const SYNC_EXPLAIN: &str = include_str!("../docs/reference/explain/sync.md");
const SYNC_TO_EXPLAIN: &str = include_str!("../docs/reference/explain/sync-to.md");
const FETCH_EXPLAIN: &str = include_str!("../docs/reference/explain/fetch.md");
const UPDATE_EXPLAIN: &str = include_str!("../docs/reference/explain/update.md");
const PUSH_EXPLAIN: &str = include_str!("../docs/reference/explain/push.md");
const PRIME_EXPLAIN: &str = include_str!("../docs/reference/explain/prime.md");
const EXPLAIN_EXPLAIN: &str = include_str!("../docs/reference/explain/explain.md");
const WORKWEAVE_EXPLAIN: &str = include_str!("../docs/reference/explain/workweave.md");
const ABORT_EXPLAIN: &str = include_str!("../docs/reference/explain/abort.md");
const ADD_EXPLAIN: &str = include_str!("../docs/reference/explain/add.md");
const REMOVE_EXPLAIN: &str = include_str!("../docs/reference/explain/remove.md");
const LOCK_EXPLAIN: &str = include_str!("../docs/reference/explain/lock.md");
const ACTIVATE_EXPLAIN: &str = include_str!("../docs/reference/explain/activate.md");
const INIT_EXPLAIN: &str = include_str!("../docs/reference/explain/init.md");

/// The complete set of verbs recognized by `rwv explain`.
///
/// This slice feeds the did-you-mean suggester. It must be kept in sync by hand
/// with the dispatch `match` arms below — the two are independent literals and
/// are not linked at compile time.
pub const KNOWN_VERBS: &[&str] = &[
    "status",
    "doctor",
    "sync",
    "sync-to",
    "fetch",
    "update",
    "push",
    "prime",
    "explain",
    "workweave",
    "abort",
    "add",
    "remove",
    "lock",
    "activate",
    "init",
];

/// Compute the Levenshtein edit distance between two strings.
///
/// Uses the standard DP approach with two alternating rows to keep memory O(n).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    // Fast paths.
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Return the closest verb from `KNOWN_VERBS` if its edit distance from
/// `input` is within the suggestion threshold, otherwise `None`.
///
/// Threshold: distance ≤ 2. This accepts single-character typos up to
/// two-character transpositions while excluding completely unrelated words
/// like "frobnicate".
fn suggest(input: &str) -> Option<&'static str> {
    const THRESHOLD: usize = 2;
    KNOWN_VERBS
        .iter()
        .map(|&v| (v, levenshtein(input, v)))
        .filter(|&(_, d)| d <= THRESHOLD)
        .min_by_key(|&(_, d)| d)
        .map(|(v, _)| v)
}

/// Print the explain bundle for the requested verb, or the index when none given.
///
/// Returns an error only when an unknown verb is requested (matches the
/// "unknown subcommand" UX agents already understand).
pub fn explain(cmd: Option<&str>) -> anyhow::Result<()> {
    match cmd {
        None => {
            print!("{INDEX_EXPLAIN}");
        }
        Some("status") => print!("{STATUS_EXPLAIN}"),
        Some("doctor") => print!("{DOCTOR_EXPLAIN}"),
        Some("sync") => print!("{SYNC_EXPLAIN}"),
        Some("sync-to") => print!("{SYNC_TO_EXPLAIN}"),
        Some("fetch") => print!("{FETCH_EXPLAIN}"),
        Some("update") => print!("{UPDATE_EXPLAIN}"),
        Some("push") => print!("{PUSH_EXPLAIN}"),
        Some("prime") => print!("{PRIME_EXPLAIN}"),
        Some("explain") => print!("{EXPLAIN_EXPLAIN}"),
        Some("workweave") => print!("{WORKWEAVE_EXPLAIN}"),
        Some("abort") => print!("{ABORT_EXPLAIN}"),
        Some("add") => print!("{ADD_EXPLAIN}"),
        Some("remove") => print!("{REMOVE_EXPLAIN}"),
        Some("lock") => print!("{LOCK_EXPLAIN}"),
        Some("activate") => print!("{ACTIVATE_EXPLAIN}"),
        Some("init") => print!("{INIT_EXPLAIN}"),
        Some(unknown) => {
            // Non-core verb: explain is reflection over core's committed,
            // CI-checked surfaces. Extending it to exec third-party binaries
            // would make rwv's reflection surface only as trustworthy as the
            // least trustworthy thing on `$PATH`, so explain never touches
            // PATH content. A close-typo hint still fires when the input is
            // within edit distance of a core verb — that's an operator help,
            // not a plugin dispatch. Any other name is redirected to the
            // plugin's own `--help`, which is the plugin's responsibility to
            // document.
            if let Some(candidate) = suggest(unknown) {
                anyhow::bail!(
                    "no explain entry for '{unknown}'; did you mean: {candidate}? \
                     Try `rwv explain` for the full index."
                );
            } else {
                anyhow::bail!("external command; try `rwv {unknown} --help`");
            }
        }
    }
    Ok(())
}
