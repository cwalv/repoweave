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
        Some(unknown) => {
            anyhow::bail!("no explain entry for '{unknown}'; try `rwv explain` for the index");
        }
    }
    Ok(())
}
