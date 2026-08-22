//! One token string, one condition, across every register that publishes one.
//!
//! Four vocabularies reach an operator as stable kebab-case tokens:
//! `RefusalKind`, `CheckViolation`'s doctor kinds, `IssueKind`'s tags, and
//! `VcsErrorOutput`'s wire kinds. A token appearing in two of them is either a
//! deliberate reuse — one condition named the same way wherever it surfaces —
//! or an accident, and the two are indistinguishable by reading either register
//! alone.
//!
//! **This is the direction that was missing.** Each register's own tests assert
//! its own spellings. A shared token could be renamed on the *doctor* side with
//! nothing red anywhere, because no assertion compared the registers to each
//! other. That is not a hypothetical: renaming a shared token on the doctor
//! side was green across the whole suite before this file existed, which is why
//! the reuse set is asserted here as a set rather than checked pair by pair
//! inside one register's own tests.
//!
//! Structural, under docs/internals/testing.md's licence 2 — a prohibition over an enumerable
//! population. The populations are read out of the source that declares them,
//! never listed here; the one thing this file states is *intent*, which is the
//! one thing source cannot tell you (see [`INTENDED_REUSE`]).
//!
//! **Scope.** The four registers above, parsed from `src/refusal.rs`,
//! `src/check.rs`, `src/integration.rs` and `src/vcs.rs`. A register minted
//! somewhere else, or a token built by concatenation rather than written as a
//! literal, is invisible here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Tokens deliberately shared between registers, and the condition each names.
///
/// This is the file's only hand-written list, and it has to be: whether two
/// registers naming one string is intent or accident is not recoverable from
/// the source. A new shared token reds this until someone records which it is,
/// which is the whole point — the failure mode being guarded is a collision
/// arriving unnoticed.
const INTENDED_REUSE: &[(&str, &str)] = &[
    (
        "clone-topology",
        "a clone sits in a topology rwv does not maintain",
    ),
    (
        "dangling-active-project",
        "the active-project pointer names a project that is not there",
    ),
    (
        "dead-op-lease",
        "an op-state lease outlived the process that took it",
    ),
    (
        "incomplete-lock",
        "the lock does not cover every manifest repo",
    ),
    (
        "legacy-manifest-format",
        "the project carries a manifest in the retired format",
    ),
    (
        "legacy-workweave-index",
        "the workweave index is in the retired format",
    ),
    (
        "legacy-workweave-marker",
        "the workweave marker is in the retired format",
    ),
    (
        "mid-operation",
        "the repository is mid-rebase, mid-merge or similar",
    ),
    (
        "missing-canonical-clone",
        "the canonical clone a checkout derives from is gone",
    ),
    (
        "missing-replay-exclusion",
        "the replay exclusion a workweave needs is absent",
    ),
    ("stale-lock", "the lock-to-HEAD relation is not `ok`"),
    (
        "unparseable-project",
        "a project directory does not parse as a project",
    ),
    (
        "unresolvable-lock-entry",
        "a lock entry names a revision that does not resolve",
    ),
    (
        "untracked-collision",
        "untracked files stand where the operation must write",
    ),
    (
        "weave-root-identity-conflict",
        "two weave roots claim one identity",
    ),
];

fn source(file: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file))
        .unwrap_or_else(|e| panic!("src/{file} is readable: {e}"))
}

/// Kebab-case tokens written as string literals on a `=>` arm.
///
/// Deliberately narrow: a `=>` arm returning a literal is how all three
/// hand-minted registers spell their tokens, and matching every string in the
/// file would sweep up prose. A register that stops using that shape becomes
/// invisible, which is what the floors below are for.
fn arm_tokens(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = body;
    while let Some(at) = rest.find("=> \"") {
        let after = &rest[at + 4..];
        if let Some(end) = after.find('"') {
            let token = &after[..end];
            if token.contains('-')
                && token
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                out.insert(token.to_owned());
            }
            rest = &after[end..];
        } else {
            break;
        }
    }
    out
}

/// The tokens `RefusalKind` publishes: `rename_all = "kebab-case"` over each
/// variant name, **except** where an explicit `#[serde(rename = "…")]`
/// overrides it.
///
/// Honouring the override is not a nicety. A variant attribute is how a token
/// is renamed without touching Rust code, so a parser that reads only variant
/// names is blind to the exact edit this file exists to catch — three
/// mutations proved it by slipping past an earlier version of this function.
fn refusal_tokens() -> BTreeSet<String> {
    let src = source("refusal.rs");
    let body = src
        .split_once("pub enum RefusalKind {")
        .expect("the enum is declared")
        .1;
    let body = body.split_once("\n}").expect("the enum closes").0;
    let mut out = BTreeSet::new();
    let mut override_token: Option<String> = None;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("#[serde(rename = \"") {
            if let Some(end) = rest.find('"') {
                override_token = Some(rest[..end].to_owned());
            }
            continue;
        }
        let Some(name) = line.strip_prefix("    ").and_then(|l| l.strip_suffix(',')) else {
            continue;
        };
        if name.is_empty()
            || !name.starts_with(char::is_uppercase)
            || !name.chars().all(|c| c.is_ascii_alphanumeric())
        {
            continue;
        }
        out.insert(override_token.take().unwrap_or_else(|| kebab(name)));
    }
    out
}

fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.char_indices() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn registers() -> BTreeMap<&'static str, BTreeSet<String>> {
    BTreeMap::from([
        ("refusal", refusal_tokens()),
        ("doctor", arm_tokens(&source("check.rs"))),
        ("issue", arm_tokens(&source("integration.rs"))),
        ("vcs", arm_tokens(&source("vcs.rs"))),
    ])
}

/// Every register is read and none of them came back empty or implausibly
/// small. A parser that quietly stopped matching would make every assertion
/// below vacuous while leaving them green.
#[test]
fn the_register_walk_is_not_vacuous() {
    let regs = registers();
    let floors = [
        ("refusal", 90usize),
        ("doctor", 30),
        ("issue", 8),
        ("vcs", 10),
    ];
    for (name, floor) in floors {
        let found = regs[name].len();
        assert!(
            found >= floor,
            "the {name} register walk yielded {found} tokens, below the floor of {floor}; \
             it has stopped reading that register"
        );
    }
}

/// The D1 sentence, asserted between registers rather than within one.
#[test]
fn a_token_in_two_registers_is_a_recorded_reuse() {
    let regs = registers();
    let mut shared: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (name, tokens) in &regs {
        for token in tokens {
            shared.entry(token.clone()).or_default().push(name);
        }
    }
    let shared: BTreeMap<String, Vec<&str>> =
        shared.into_iter().filter(|(_, r)| r.len() > 1).collect();

    let recorded: BTreeSet<&str> = INTENDED_REUSE.iter().map(|(t, _)| *t).collect();
    let found: BTreeSet<&str> = shared.keys().map(|s| s.as_str()).collect();

    let unrecorded: Vec<&&str> = found.difference(&recorded).collect();
    assert!(
        unrecorded.is_empty(),
        "these tokens are published by more than one register with nothing saying that is \
         deliberate. If it is a shared condition, record it; if two conditions have collided \
         on one name, rename one of them:\n{unrecorded:#?}\n(registers: {shared:#?})"
    );

    let vanished: Vec<&&str> = recorded.difference(&found).collect();
    assert!(
        vanished.is_empty(),
        "these reuses are recorded but no longer shared — one side was renamed or removed, \
         which is the drift this file exists to catch:\n{vanished:#?}"
    );
}

/// Two tokens one edit apart, naming different conditions, are a trap for a
/// reader who met one in a terminal and is typing it back.
///
/// Reported over the whole universe rather than per register, because the
/// collision that matters is between vocabularies an operator does not know are
/// separate.
#[test]
fn no_two_conditions_sit_one_edit_apart() {
    let regs = registers();
    let recorded: BTreeSet<&str> = INTENDED_REUSE.iter().map(|(t, _)| *t).collect();
    let universe: BTreeSet<String> = regs.values().flatten().cloned().collect();
    assert!(
        universe.len() >= 130,
        "the universe walk yielded {} tokens; it has stopped reading",
        universe.len()
    );

    let tokens: Vec<&String> = universe.iter().collect();
    let mut near = Vec::new();
    for (i, a) in tokens.iter().enumerate() {
        for b in &tokens[i + 1..] {
            if recorded.contains(a.as_str()) && recorded.contains(b.as_str()) {
                continue;
            }
            if edit_distance(a, b) <= 1 {
                near.push(format!("{a} / {b}"));
            }
        }
    }
    assert!(
        near.is_empty(),
        "these token pairs are one edit apart and name different conditions:\n{}",
        near.join("\n")
    );
}

fn edit_distance(a: &str, b: &str) -> usize {
    if a.len().abs_diff(b.len()) > 1 {
        return 2;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut curr = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            curr.push(
                (prev[j + 1] + 1)
                    .min(curr[j] + 1)
                    .min(prev[j] + usize::from(ca != cb)),
            );
        }
        prev = curr;
    }
    prev[b.len()]
}

/// Tokens that reach an operator but that `rwv explain` cannot serve.
///
/// An entry is served by slicing a heading titled *exactly* `` `token` ``, so a
/// vocabulary documented some other way — or not at all — is printed at people
/// without being reachable by the command the tooling tells them to run.
///
/// **No entry anywhere, though a bare grep says otherwise.** The VCS wire
/// kinds not shared with the refusal register. A plain `git grep` for one of
/// these finds six or more files and reads like documentation; every hit is
/// a generated JSON schema — standalone under `docs/reference/schemas/`, or
/// inlined into a `docs/reference/explain/*.md` bundle inside an
/// `"enum": [...]` block — plus one `docs/internals/` page, which is not
/// operator-facing. None is an entry. The query that decides it asks for the
/// heading rather than the string:
///
/// ```sh
/// git grep '^#\+ `<token>`' -- docs/
/// ```
///
/// which returns nothing for any of them. `uncommitted-changes` is in this
/// group and carries a second, separate problem: no code path can emit it.
///
/// Recorded rather than fixed — closing it is a change to pages this file does
/// not own. The set is asserted exactly, so a vocabulary gaining an entry, or a
/// new one arriving unservable, reds here and is re-decided rather than
/// absorbed.
const NOT_EXPLAIN_SERVABLE: &[&str] = &[
    // VCS wire kinds — no entry heading; a bare grep finds only schemas
    "branch-already-exists",
    "cherry-pick",
    "command-failed",
    "hook-rejected",
    "not-a-repo",
    "rebase-conflict",
    "revision-not-found",
    "stale-ref-witness",
    "uncommitted-changes",
    "worktree-exists",
];

/// Every token any register publishes is served by `rwv explain`, except the
/// recorded set above — and that set is exactly the unservable ones.
#[test]
fn every_published_token_is_servable_or_a_recorded_exception() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference");
    let pages = format!(
        "{}\n{}",
        std::fs::read_to_string(docs.join("refusals.md")).expect("refusals published"),
        std::fs::read_to_string(docs.join("doctor-findings.md")).expect("findings published"),
    );
    let served: BTreeSet<&str> = pages
        .lines()
        .filter_map(|line| {
            let hashes = line.len() - line.trim_start_matches('#').len();
            let rest = line.get(hashes..)?.trim();
            (hashes >= 2 && rest.starts_with('`') && rest.ends_with('`') && rest.len() > 2)
                .then(|| rest.trim_matches('`'))
        })
        .filter(|t| !t.contains(' '))
        .collect();
    assert!(
        served.len() >= 90,
        "the entry walk yielded {} headings; it has stopped reading the pages",
        served.len()
    );

    let published: BTreeSet<String> = registers().values().flatten().cloned().collect();
    let recorded: BTreeSet<&str> = NOT_EXPLAIN_SERVABLE.iter().copied().collect();

    let unservable: BTreeSet<&str> = published
        .iter()
        .map(|s| s.as_str())
        .filter(|t| !served.contains(t))
        .collect();

    let newly_unservable: Vec<&&str> = unservable.difference(&recorded).collect();
    assert!(
        newly_unservable.is_empty(),
        "these tokens are printed to operators but `rwv explain` cannot serve them:\n{newly_unservable:#?}"
    );
    let now_servable: Vec<&&str> = recorded.difference(&unservable).collect();
    assert!(
        now_servable.is_empty(),
        "these are recorded as unservable but now have an entry — drop them from the record:\n{now_servable:#?}"
    );
}
