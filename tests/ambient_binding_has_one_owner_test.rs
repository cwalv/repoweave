//! The `.rwv-active` pointer is read at the dispatch boundary and nowhere
//! else. This scan pins that as a module boundary rather than a habit.
//!
//! The rule the source is meant to satisfy: a workspace's pointer is mutable
//! ambient state, so it decides the project once, for the invocation, in the
//! resolution chain — and from there the binding travels as data (a
//! `ProjectName` by value, or the project recorded on an op). Three bugs came
//! from a resolve API that spelled "ask this workspace what it is active on"
//! and "give me this workspace as my operation sees it" identically, so the
//! wrong one compiled, read as innocuous, and was byte-identical to the right
//! one at every invocation site.
//!
//! What is pinned, and who owns each needle:
//!
//!   - `resolve_invocation` — the resolution chain itself, the only entry
//!     point that may consult the pointer. Owned by `workspace.rs`, which
//!     defines it, `cli/dispatch.rs`, which is the boundary, and the three
//!     bootstrap sites that resolve a workspace they just created.
//!   - `read_active_project` — the pointer accessor. Owned by `workspace.rs`
//!     plus the two verbs whose subject IS the pointer: `check.rs` (doctor
//!     reports and repairs a dangling one) and `fetch.rs` (bootstrap).
//!   - a path *assembled* to the pointer file — `join(".rwv-active")` — which
//!     reads it without going through the accessor and so is invisible to the
//!     needle above. No production line does this today; the check is a proven
//!     negative, and the seeded test is what keeps the zero meaningful.
//!     Merely naming the file is not a read: `check.rs` says `.rwv-active`
//!     throughout, because a doctor finding that repairs the file has to name
//!     it, and none of those lines are flagged.
//!
//! A hit outside an owner is op code reaching for ambient state — the shape
//! that rolled a sync-to's savepoints back in the wrong project's repos, that
//! made abort report a clean rollback it never performed, and that made a pull
//! read a sibling project's lock.
//!
//! Residue, for anyone extending this. It inherits `src_scan`'s filters, and
//! both matter here: line-leading `//` comments are dropped, so a doc comment
//! naming `resolve_invocation` (including one clap lifts onto `--help`) is
//! invisible and can go stale; `#[cfg(test)]` items are skipped by brace
//! depth, so the calls in `prime.rs`, `sync.rs` and `workspace.rs`'s own test
//! modules are deliberately out of scope — a test resolving a fixture
//! workspace is not op code. It matches written names only: a resolution
//! reached through an alias, a re-export, or a function pointer is not seen,
//! and a path built by `format!` rather than `join` slips the third check.
//! Owner lists are per FILE, so a new ambient read inside `check.rs` or
//! `dispatch.rs` — the files that legitimately hold several — is not reported.
//!
//! What it cannot see at all is the residue the design named for itself: a
//! `resolve_for_project` call passed the WRONG binding still compiles. That
//! failure is visible data flow at the call site rather than a silent default,
//! which is the trade this boundary buys — and it is the reason the pin's own
//! escalation trigger is a compiler-enforced binding type, not a wider scan.

mod common;

use common::src_scan::{production_lines, SourceLine};

/// The resolution chain, its accessor, and the file both are about.
const CHAIN: &str = "resolve_invocation";
const ACCESSOR: &str = "read_active_project";
const POINTER_FILE: &str = ".rwv-active";

/// Files that may run the resolution chain: its definition, the dispatch
/// boundary, and the bootstrap verbs that resolve a workspace they created.
const CHAIN_OWNERS: &[&str] = &[
    "workspace.rs",
    "cli/dispatch.rs",
    "init.rs",
    "fetch.rs",
    "workweave.rs",
];

/// Files that may read the pointer directly: its owner, plus the two verbs
/// whose subject is the pointer rather than the work done through it.
const ACCESSOR_OWNERS: &[&str] = &["workspace.rs", "check.rs", "fetch.rs"];

fn sites<'a>(lines: &'a [SourceLine], needle: &str) -> Vec<&'a SourceLine> {
    lines.iter().filter(|l| l.text.contains(needle)).collect()
}

fn strays(lines: &[SourceLine], needle: &str, owners: &[&str]) -> Vec<String> {
    sites(lines, needle)
        .iter()
        .filter(|l| !owners.contains(&l.file.as_str()))
        .map(|l| format!("{}: {}", l.site(), l.text.trim()))
        .collect()
}

#[test]
fn the_scan_is_pointed_at_a_whole_source_tree() {
    let lines = production_lines();
    assert!(
        lines.len() >= 10_000,
        "expected at least 10000 production lines under src/, got {} — an \
         all-sites-in-one-file result below would be measuring the corpus, \
         not the source",
        lines.len()
    );
}

#[test]
fn the_resolution_chain_runs_only_at_the_dispatch_boundary() {
    let lines = production_lines();
    let found = sites(&lines, CHAIN);
    assert!(
        found.len() >= 2,
        "`{CHAIN}` was found {} time(s) under src/. It has a definition and \
         callers — too few means the needle stopped matching and the stray \
         check below would pass over an empty set",
        found.len()
    );

    let strays = strays(&lines, CHAIN, CHAIN_OWNERS);
    assert!(
        strays.is_empty(),
        "`{CHAIN}` consults `{POINTER_FILE}`, so it belongs to the invocation \
         boundary and the bootstrap sites ({CHAIN_OWNERS:?}). A call elsewhere \
         is op code deriving a binding it should have been handed — use \
         `resolve_for_project` with the project already in scope. Found: \
         {strays:?}"
    );
}

#[test]
fn the_pointer_is_read_only_by_its_owner_and_the_verbs_about_it() {
    let lines = production_lines();
    let found = sites(&lines, ACCESSOR);
    assert!(
        !found.is_empty(),
        "`{ACCESSOR}` was not found anywhere under src/; the accessor has to \
         exist for this boundary to mean anything"
    );

    let strays = strays(&lines, ACCESSOR, ACCESSOR_OWNERS);
    assert!(
        strays.is_empty(),
        "`{ACCESSOR}` reads ambient state that decides nothing after the \
         invocation. Owners: {ACCESSOR_OWNERS:?}. Found: {strays:?}"
    );
}

/// The accessor is `pub`, so a caller can always bypass it by joining the
/// filename itself. No production line does — the owner reaches the file
/// through its own private constant — and this keeps it that way.
///
/// A proven negative, deliberately: the corpus yields nothing, so the
/// assertion below cannot distinguish "clean" from "matcher broken" on its
/// own. [`a_chain_call_in_op_code_is_what_this_reports`] plants one and
/// requires this same predicate to report it, which is what makes the zero
/// mean something.
#[test]
fn nothing_assembles_a_path_to_the_pointer_by_hand() {
    let hand_rolled = hand_rolled_pointer_paths(&production_lines());
    assert!(
        hand_rolled.is_empty(),
        "a path to `{POINTER_FILE}` assembled at the call site reads the \
         pointer without going through `{ACCESSOR}`, so neither that needle nor \
         a reviewer looking for it would see the read. Go through the accessor. \
         Found: {hand_rolled:?}"
    );
}

/// Lines that build a path to the pointer file rather than merely naming it.
/// Operator-facing text says `.rwv-active` all over `check.rs` — a doctor
/// finding that repairs the file has to name it — and that is not a read.
fn hand_rolled_pointer_paths(lines: &[SourceLine]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.text.contains(POINTER_FILE) && l.text.contains("join("))
        .map(|l| format!("{}: {}", l.site(), l.text.trim()))
        .collect()
}

/// The seeded failure: the scan must report an op-code site, and must not
/// report the bound resolution that is the correct shape at the same place.
#[test]
fn a_chain_call_in_op_code_is_what_this_reports() {
    let planted = |file: &str, text: &str| SourceLine {
        file: file.to_string(),
        line: 1,
        text: text.to_string(),
    };
    let corpus =
        vec![
        planted(
            "workspace.rs",
            "    pub fn resolve_invocation(cwd: &Path, project_flag: Option<ProjectName>)",
        ),
        planted(
            "cli/dispatch.rs",
            "        WorkspaceContext::resolve_invocation(origin_dir, project_override)?,",
        ),
        planted(
            "sync.rs",
            "        let source_ctx = WorkspaceContext::resolve_invocation(&source_dir, None)?;",
        ),
        planted(
            "sync.rs",
            "        let ctx = WorkspaceContext::resolve_for_project(&dir, &cwd_project_name)?;",
        ),
        planted("check.rs", "    if let Some(a) = read_active_project(root) {"),
        planted("sync.rs", "    let p = read_active_project(root);"),
        planted(
            "check.rs",
            "        \"{}: carries both `.rwv-active` (which {pointer}) and \\",
        ),
        planted("sync.rs", "    let p = root.join(\".rwv-active\");"),
    ];

    let chain_strays = strays(&corpus, CHAIN, CHAIN_OWNERS);
    assert_eq!(
        chain_strays.len(),
        1,
        "exactly the seeded op-code chain call must be reported; got {chain_strays:?}"
    );
    assert!(
        chain_strays[0].contains("sync.rs") && chain_strays[0].contains("&source_dir"),
        "the reported site must be the seeded one: {chain_strays:?}"
    );
    assert!(
        !chain_strays[0].contains("resolve_for_project"),
        "the bound resolution is the shape this scan is asking for and must \
         never be reported: {chain_strays:?}"
    );

    let accessor_strays = strays(&corpus, ACCESSOR, ACCESSOR_OWNERS);
    assert_eq!(
        accessor_strays.len(),
        1,
        "the seeded pointer read outside an owner must be reported, and \
         check.rs's must not; got {accessor_strays:?}"
    );
    assert!(
        accessor_strays[0].contains("sync.rs"),
        "the reported pointer read must be the seeded one: {accessor_strays:?}"
    );

    let hand_rolled = hand_rolled_pointer_paths(&corpus);
    assert_eq!(
        hand_rolled.len(),
        1,
        "the seeded hand-assembled path must be reported and the operator text \
         naming the same file must not — that separation is the whole predicate; \
         got {hand_rolled:?}"
    );
    assert!(
        hand_rolled[0].contains("join("),
        "the reported line must be the assembled path: {hand_rolled:?}"
    );
}
