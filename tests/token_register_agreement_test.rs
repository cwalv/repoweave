//! One token string, one condition, across every register that publishes one.
//!
//! Five vocabularies name a condition to an operator as a stable kebab-case
//! token: `RefusalKind`, `CheckViolation`'s doctor kinds, `IssueKind`'s tags,
//! `VcsErrorOutput`'s wire kinds, and `SyncFailureOutput`'s. A token appearing
//! in two of them is either a deliberate reuse — one condition named the same
//! way wherever it surfaces — or an accident, and the two are
//! indistinguishable by reading either register alone.
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
//! **Scope.** The five registers above, parsed from `src/refusal.rs`,
//! `src/check.rs`, `src/integration.rs`, `src/vcs.rs` and `src/sync.rs` — the
//! four match-minted ones read from the body of the one function that mints
//! each, so every arm is in range whatever it is spelled like. A token built by
//! concatenation is still invisible; one written as a constant is resolved.
//!
//! **A field value is not a condition.** `ConflictOp` serialises `rebase` /
//! `merge` / `cherry-pick` into `--json`, but as the `op` field *inside* the
//! `rebase-conflict` condition rather than as a `kind` discriminant. It answers
//! which operation was in flight; it never names what happened, so it earns no
//! `rwv explain` entry and is not a register here. It is still walked, for one
//! property: an op name must not also be a condition name, or a token read off
//! a machine surface stops identifying which vocabulary it came from. That is
//! [`an_op_name_is_not_also_a_condition_name`], and the distinction rests on
//! that assertion rather than on the paragraph you are reading. A boundary
//! written down here and held nowhere is worth about as much as it costs to
//! type: `cherry-pick` was carried in the VCS register by an earlier walk, and
//! no prose was going to notice.
//!
//! **Residue, now walked for one property.** More kebab vocabularies reach
//! `--json` than the five above — outcome tags, drift classifications, lock
//! and containment relations. None of them earns an `rwv explain` entry, and
//! that part of the boundary still costs nothing. The disjointness property is
//! the part that does, and
//! [`every_unwalked_kebab_vocabulary_is_disjoint_from_conditions`] now holds
//! it for all of them, not just `ConflictOp` — reading
//! `docs/reference/schemas/*.json` (`generate-explain`'s own drift-gated
//! output, so a stale artifact here is a stale artifact everywhere else that
//! reads one) rather than source, because these vocabularies are not
//! hand-parsed match arms the way the five registers above are. Vocab-vocab
//! lexical sharing among them (`failed` in four, `ahead`/`behind`/`diverged`
//! in two, `safe-to-fix` in two more) is deliberately not recorded as an
//! [`INTENDED_REUSE`] entry: every one of these tokens sits on its own
//! positionally-fixed field (`kind`, `relation`, …), so which vocabulary
//! published a shared string is never ambiguous the way a condition name —
//! read out of context, in an error message or an `rwv explain` argument —
//! would be.
//!
//! Widening the walk to the full population did find a real collision, not a
//! vacuous pass: four of the doctor sub-kind vocabularies deliberately mirror
//! seven `RefusalKind` conditions — the same D1 sentence one level below the
//! five registers, recorded in [`INTENDED_SUBKIND_REUSE`] rather than
//! [`INTENDED_REUSE`] because a sub-kind is not a token any of the five
//! registers mints.

mod common;

use common::json_schema;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Tokens deliberately shared between registers, the exact set of registers
/// each is shared across, and the condition each names.
///
/// This is the file's only hand-written list, and it has to be: whether two
/// registers naming one string is intent or accident is not recoverable from
/// the source, and neither is which registers were meant to share it. A new
/// shared token reds this until someone records which it is; a register
/// outside the recorded set picking up an already-recorded token reds it too
/// — the failure mode being guarded is a collision arriving unnoticed, and a
/// third register silently joining a recorded two-register reuse is one.
const INTENDED_REUSE: &[(&str, &[&str], &str)] = &[
    (
        "clone-topology",
        &["doctor", "refusal"],
        "a clone sits in a topology rwv does not maintain",
    ),
    (
        "dangling-active-project",
        &["doctor", "refusal"],
        "the active-project pointer names a project that is not there",
    ),
    (
        "dead-op-lease",
        &["doctor", "refusal"],
        "an op-state lease outlived the process that took it",
    ),
    (
        "head-unreadable",
        &["doctor", "sync"],
        "a repo's HEAD could not be read",
    ),
    (
        "incomplete-lock",
        &["doctor", "refusal"],
        "the lock does not cover every manifest repo",
    ),
    (
        "legacy-manifest-format",
        &["doctor", "refusal"],
        "the project carries a manifest in the retired format",
    ),
    (
        "legacy-workweave-index",
        &["doctor", "refusal"],
        "the workweave index is in the retired format",
    ),
    (
        "legacy-workweave-marker",
        &["doctor", "refusal"],
        "the workweave marker is in the retired format",
    ),
    (
        "mid-operation",
        &["refusal", "vcs"],
        "the repository is mid-rebase, mid-merge or similar",
    ),
    (
        "missing-canonical-clone",
        &["doctor", "refusal"],
        "the canonical clone a checkout derives from is gone",
    ),
    (
        "missing-replay-exclusion",
        &["doctor", "refusal"],
        "the replay exclusion a workweave needs is absent",
    ),
    (
        "stale-lock",
        &["doctor", "refusal"],
        "the lock-to-HEAD relation is not `ok`",
    ),
    (
        "unparseable-project",
        &["doctor", "refusal"],
        "a project directory does not parse as a project",
    ),
    (
        "unresolvable-lock-entry",
        &["doctor", "refusal"],
        "a lock entry names a revision that does not resolve",
    ),
    (
        "untracked-collision",
        &["refusal", "vcs"],
        "untracked files stand where the operation must write",
    ),
    (
        "weave-root-identity-conflict",
        &["doctor", "refusal"],
        "two weave roots claim one identity",
    ),
];

fn source(file: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file))
        .unwrap_or_else(|e| panic!("src/{file} is readable: {e}"))
}

/// The body of the one function that mints a register's tokens, from its
/// signature to the closing brace at the same indent.
///
/// Crude and keyed to how these three files are written — a minting function
/// sits at four-space indent inside an `impl`, so `    }` ends it. A register
/// that moves out of that shape yields nothing here, which
/// [`the_register_walk_is_not_vacuous`] is what catches.
fn minting_fn<'a>(source: &'a str, signature: &str) -> &'a str {
    let from = source
        .find(signature)
        .unwrap_or_else(|| panic!("`{signature}` is declared"));
    let body = &source[from..];
    let end = body.find("\n    }").map_or(body.len(), |at| at + 6);
    &body[..end]
}

/// The kebab-case tokens a minting function writes on its `=>` arms.
///
/// **Scoped to that function, and that is what makes it total.** An earlier
/// version scanned the whole file and had to guess which `=> "literal"` arms
/// were tokens; it guessed by requiring a hyphen, which silently dropped every
/// single-word token a register published. Measured over the three files this
/// reads, that heuristic was hiding `provenance` from the doctor register,
/// `surfacing` from the issue register and `io` from the VCS register — three
/// published tokens, none of them recorded anywhere as unwalked, with every
/// assertion in this file green. Dropping the hyphen rule without narrowing the
/// scope is not the fix either: over the same files it admits a `Severity`
/// display arm and two arms of a `Display` impl, which are not tokens.
///
/// A token minted through a `Self::CONST` rather than written inline is
/// resolved against the constant's own declaration — a register is free to name
/// a token it also needs as a value, and a walk that reads only literals cannot
/// see that arm at all.
fn arm_tokens(source: &str, signature: &str) -> BTreeSet<String> {
    let body = minting_fn(source, signature);
    let mut out = BTreeSet::new();
    for arm in body.split("=>").skip(1) {
        let token = match arm.trim_start().strip_prefix('"') {
            Some(rest) => match rest.find('"') {
                Some(end) => rest[..end].to_owned(),
                None => continue,
            },
            None => {
                let Some(name) = arm
                    .trim_start()
                    .strip_prefix("Self::")
                    .map(|r| {
                        r.chars()
                            .take_while(|c| {
                                c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_'
                            })
                            .collect::<String>()
                    })
                    .filter(|n| !n.is_empty())
                else {
                    continue;
                };
                let decl = format!("const {name}: &'static str = \"");
                let Some(at) = source.find(&decl) else {
                    continue;
                };
                let rest = &source[at + decl.len()..];
                match rest.find('"') {
                    Some(end) => rest[..end].to_owned(),
                    None => continue,
                }
            }
        };
        if !token.is_empty()
            && token
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            out.insert(token);
        }
    }
    out
}

/// How many arms a minting function has, for comparison against how many
/// tokens the walk recovered from it.
///
/// The pin a floor cannot be. A floor catches a register going dark wholesale;
/// it cannot catch one going *partially* blind, which is the likelier drift and
/// the one that reads green — the issue register's floor is well under its size,
/// so it passed for as long as two of its arms were invisible.
fn arm_count(source: &str, signature: &str) -> usize {
    minting_fn(source, signature).matches("=>").count()
}

/// The tokens a `rename_all = "kebab-case"` enum publishes: its variant names
/// kebabbed, **except** where an explicit `#[serde(rename = "…")]` overrides
/// one.
///
/// Honouring the override is not a nicety. A variant attribute is how a token
/// is renamed without touching Rust code, so a parser that reads only variant
/// names is blind to the exact edit this file exists to catch — three
/// mutations proved it by slipping past an earlier version of this function.
///
/// Fieldless variants only, which is what both callers declare. A variant
/// carrying fields spans lines this yields nothing for, so a register that
/// grows one goes partially blind — the reason each caller is pinned against a
/// count it does not compute itself.
fn serde_enum_tokens(src: &str, decl: &str) -> BTreeSet<String> {
    let body = src
        .split_once(decl)
        .unwrap_or_else(|| panic!("`{decl}` is declared"))
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

fn refusal_tokens() -> BTreeSet<String> {
    serde_enum_tokens(&source("refusal.rs"), "pub enum RefusalKind {")
}

/// The op names `ConflictOp` puts on the wire, read from the declaration that
/// serialises them rather than from the `Display` impl that spells them a
/// second time.
///
/// Reading `Display` would be reading the wrong producer: a `#[serde(rename)]`
/// moves the wire token and leaves the `Display` arm untouched. The two are
/// compared in [`an_op_name_is_not_also_a_condition_name`], which is what makes
/// the second spelling safe to keep.
fn conflict_op_tokens() -> BTreeSet<String> {
    serde_enum_tokens(&source("vcs.rs"), "pub enum ConflictOp {")
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

/// The hand-minted registers, each as the file that declares it and the
/// signature of the one function that mints its tokens.
///
/// `sync` is the one whose minting function is not itself the wire producer:
/// `rwv sync --json` serialises `SyncFailureOutput` and never calls
/// `SyncFailure::kind`. Reading the match arms is faithful only because
/// `sync_failure_kind_matches_wire_tag` pins the two spellings against each
/// other over every variant, so a rename on the wire side cannot pass while the
/// arms stand still.
const MINTED_REGISTERS: &[(&str, &str, &str)] = &[
    (
        "doctor",
        "check.rs",
        "pub fn wire_kind(&self) -> &'static str {",
    ),
    (
        "issue",
        "integration.rs",
        "pub fn tag(&self) -> &'static str {",
    ),
    ("vcs", "vcs.rs", "pub fn kind(&self) -> &'static str {"),
    ("sync", "sync.rs", "pub fn kind(&self) -> &'static str {"),
];

fn registers() -> BTreeMap<&'static str, BTreeSet<String>> {
    let mut out = BTreeMap::from([("refusal", refusal_tokens())]);
    for (name, file, signature) in MINTED_REGISTERS {
        out.insert(name, arm_tokens(&source(file), signature));
    }
    out
}

/// Every arm of a minting function yielded a token.
///
/// The direction a floor cannot assert. `arm_tokens` recovers what it can
/// recognise, so a register that changes how one arm is spelled loses that
/// token silently and every comparison downstream narrows by one — while the
/// walk still returns plenty and the floor still passes. Comparing the count
/// against the arms in range is what makes the walk total rather than merely
/// non-empty.
#[test]
fn every_arm_of_every_minting_function_yields_a_token() {
    for (name, file, signature) in MINTED_REGISTERS {
        let source = source(file);
        let arms = arm_count(&source, signature);
        let tokens = arm_tokens(&source, signature);
        assert!(
            arms > 0,
            "the {name} register's minting function was sliced to nothing"
        );
        assert_eq!(
            tokens.len(),
            arms,
            "the {name} register's minting function in src/{file} has {arms} arms and the \
             walk recovered {} tokens — an arm is spelled in a way the walk does not read, \
             and everything this file asserts about that register is short by one:\n{tokens:#?}",
            tokens.len()
        );
    }
}

/// Every register is read and none of them came back empty or implausibly
/// small. A parser that quietly stopped matching would make every assertion
/// below vacuous while leaving them green.
///
/// **What a floor does not do**, because the wrong answer here cost this file
/// two published tokens: it catches a register going dark wholesale, not one
/// going partially blind. A floor is by construction below the register's real
/// size, so a walk that loses one arm — or four — still clears it. The
/// partial case belongs to
/// [`every_arm_of_every_minting_function_yields_a_token`], which compares the
/// count against the arms actually in range; this floor is what stands behind
/// `refusal`, whose parser reads an enum declaration rather than match arms and
/// so has no arm count to compare against.
#[test]
fn the_register_walk_is_not_vacuous() {
    let regs = registers();
    let floors = [
        ("refusal", 90usize),
        ("doctor", 30),
        ("issue", 8),
        // Lowered from 10 when three variants nothing constructed were retired
        // off the wire. A floor tracks how many tokens a register is known to
        // have, so it moves when the register does; it is not a claim that the
        // register may not shrink.
        ("vcs", 8),
        ("sync", 3),
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

/// `ConflictOp`'s vocabulary is exempt from needing an `rwv explain` entry, and
/// this is the property that exemption rests on.
///
/// An op name is a field value, so an operator meets it beside a condition
/// token in one JSON object and has only the spelling to tell them apart. The
/// exemption is safe exactly while the two vocabularies are disjoint; a
/// condition minted as `merge` would make `rwv explain merge` owe an answer
/// that names the wrong kind of thing.
///
/// The count and the `Display` arms are what keep this from passing vacuously.
/// A parser that stopped reading the declaration would assert disjointness over
/// nothing, and a `#[serde(rename)]` that moved a wire token would leave the
/// second spelling behind — the failure mode that put an op name in the VCS
/// register once, read off `Display` by a walk that had no business there.
#[test]
fn an_op_name_is_not_also_a_condition_name() {
    let vcs = source("vcs.rs");
    let ops = conflict_op_tokens();
    let display = "impl std::fmt::Display for ConflictOp {";
    let spelled = arm_tokens(&vcs, display);
    let arms = arm_count(&vcs, display);
    assert_eq!(
        ops.len(),
        arms,
        "ConflictOp declares {} serialised op names against {arms} `Display` arms; one \
         producer moved without the other, or the declaration walk has gone blind:\n{ops:#?}",
        ops.len()
    );
    assert_eq!(
        ops, spelled,
        "ConflictOp's wire spelling and its `Display` spelling have diverged, so a caller \
         composing a message from `Display` names an op the wire does not"
    );

    let conditions: BTreeSet<String> = registers().values().flatten().cloned().collect();
    let collided: Vec<&String> = ops.iter().filter(|op| conditions.contains(*op)).collect();
    assert!(
        collided.is_empty(),
        "these name an in-flight operation on one surface and a condition on another, and \
         an operator reading one off `--json` cannot tell which:\n{collided:#?}"
    );
}

/// The D1 sentence, asserted between registers rather than within one.
#[test]
fn a_token_in_two_registers_is_a_recorded_reuse() {
    let regs = registers();
    let mut shared: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for (name, tokens) in &regs {
        for token in tokens {
            shared.entry(token.clone()).or_default().insert(name);
        }
    }
    let shared: BTreeMap<String, BTreeSet<&str>> =
        shared.into_iter().filter(|(_, r)| r.len() > 1).collect();

    let recorded: BTreeMap<&str, BTreeSet<&str>> = INTENDED_REUSE
        .iter()
        .map(|(token, registers, _)| (*token, registers.iter().copied().collect()))
        .collect();

    let found: BTreeSet<&str> = shared.keys().map(|s| s.as_str()).collect();
    let recorded_tokens: BTreeSet<&str> = recorded.keys().copied().collect();

    let unrecorded: Vec<&&str> = found.difference(&recorded_tokens).collect();
    assert!(
        unrecorded.is_empty(),
        "these tokens are published by more than one register with nothing saying that is \
         deliberate. If it is a shared condition, record it; if two conditions have collided \
         on one name, rename one of them:\n{unrecorded:#?}\n(registers: {shared:#?})"
    );

    let vanished: Vec<&&str> = recorded_tokens.difference(&found).collect();
    assert!(
        vanished.is_empty(),
        "these reuses are recorded but no longer shared — one side was renamed or removed, \
         which is the drift this file exists to catch:\n{vanished:#?}"
    );

    let widened: Vec<(&str, &BTreeSet<&str>, &BTreeSet<&str>)> = recorded
        .iter()
        .filter_map(|(token, claimed)| {
            let actual = &shared[*token];
            (actual != claimed).then_some((*token, claimed, actual))
        })
        .collect();
    assert!(
        widened.is_empty(),
        "these tokens are shared by a register set other than the one recorded — a register \
         outside it has picked up the token, or one inside it dropped it (token, recorded, \
         actual):\n{widened:#?}"
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
    let recorded: BTreeSet<&str> = INTENDED_REUSE.iter().map(|(t, _, _)| *t).collect();
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
/// **Empty, and that is the assertion.** Every token any register publishes is
/// served. This list exists so that stops being true loudly: a vocabulary that
/// arrives undocumented reds here rather than being printed at people who
/// cannot look it up.
///
/// It was not always empty. The VCS wire kinds had no entry anywhere while a
/// bare `git grep` for one of them found six or more files and read like
/// documentation — every hit a generated JSON schema, standalone under
/// `docs/reference/schemas/` or inlined into a `docs/reference/explain/*.md`
/// bundle inside an `"enum": [...]` block, plus one `docs/internals/` page that
/// is not operator-facing. The query that decides it asks for the heading
/// rather than the string:
///
/// ```sh
/// git grep '^#\+ `<token>`' -- docs/
/// ```
///
/// Run it against a control, because a query that answers nothing everywhere
/// proves nothing: it returns one for `mid-operation`, which is served.
///
/// The reachable kinds gained entries on `docs/reference/vcs-errors.md`. The
/// rest were removed from the wire: three variants no production path ever
/// constructed, so no entry could describe a state an operator could reach.
/// Documenting them would have published the fiction; retiring them is what
/// let this list reach nothing.
const NOT_EXPLAIN_SERVABLE: &[&str] = &[];

/// Every token any register publishes is served by `rwv explain`, except the
/// recorded set above — and that set is exactly the unservable ones.
#[test]
fn every_published_token_is_servable_or_a_recorded_exception() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference");
    let pages = format!(
        "{}\n{}\n{}",
        std::fs::read_to_string(docs.join("refusals.md")).expect("refusals published"),
        std::fs::read_to_string(docs.join("doctor-findings.md")).expect("findings published"),
        std::fs::read_to_string(docs.join("vcs-errors.md")).expect("vcs errors published"),
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

/// True for a wire token this file treats as kebab-case: lowercase ASCII
/// letters, digits and `-` only.
///
/// A property declared `rename_all = "snake_case"` still passes through here
/// unfiltered when every one of its members happens to be one word (`ok`,
/// `failed`) — snake_case and kebab-case coincide until a name needs a
/// separator. What this excludes is a literal underscore surviving into a
/// token (`LockRelation`'s `no_lock`), the same character class `arm_tokens`
/// already holds every hand-parsed register to.
fn is_kebab_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `prop`'s value when it is a `{"type": "string", "enum": [x]}` singleton —
/// the shape schemars gives an internally-tagged variant's own discriminant
/// field, whether or not the variant carries other fields alongside it.
fn single_value_enum(prop: &Value) -> Option<&str> {
    if prop.get("type")?.as_str()? != "string" {
        return None;
    }
    match prop.get("enum")?.as_array()?.as_slice() {
        [one] => one.as_str(),
        _ => None,
    }
}

/// The kebab tokens one `oneOf` arm (or a bare enum definition treated as its
/// own sole arm) contributes.
///
/// Three shapes, because schemars uses a different one depending on whether a
/// variant carries a doc comment and whether it carries fields: a flat
/// `{"enum": [...]}` for a run of undocumented fieldless variants
/// (`FetchOutcomeStatus`); a single-key `{"enum": [x]}` for one documented
/// fieldless variant (`UpdateKind`'s arms) or as an internally-tagged
/// variant's discriminant property alongside its other fields
/// (`PushOutcomeOutput`'s `kind`, `ContainmentVerdictOutput`'s `relation`);
/// and a single required key equal to its own sole property, for an
/// externally-tagged variant that carries data
/// (`WorkweaveTreeIntegrityKind`'s `dangling-parent`). A parser that reads
/// only the first shape is the one that under-reads every doctor sub-kind
/// enum in the schema corpus.
fn collect_arm_tokens(arm: &Value, out: &mut BTreeSet<String>) {
    if arm.get("type").and_then(Value::as_str) == Some("string") {
        for value in arm
            .get("enum")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(s) = value.as_str() {
                if is_kebab_token(s) {
                    out.insert(s.to_owned());
                }
            }
        }
        return;
    }
    if arm.get("type").and_then(Value::as_str) != Some("object") {
        return;
    }
    let props = arm.get("properties").and_then(Value::as_object);
    let mut tagged = false;
    for (_, prop) in props.into_iter().flatten() {
        if let Some(tag) = single_value_enum(prop) {
            if is_kebab_token(tag) {
                out.insert(tag.to_owned());
            }
            tagged = true;
        }
    }
    if tagged {
        return;
    }
    let required = arm.get("required").and_then(Value::as_array);
    if let (Some(required), Some(props)) = (required, props) {
        if let [Value::String(tag)] = required.as_slice() {
            if props.len() == 1 && props.contains_key(tag) && is_kebab_token(tag) {
                out.insert(tag.clone());
            }
        }
    }
}

/// A schema `definitions` entry's full kebab vocabulary: every arm of its
/// `oneOf`, or its own `enum` when it has no `oneOf` at all.
fn definition_tokens(def: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if def.get("type").and_then(Value::as_str) == Some("string") {
        collect_arm_tokens(def, &mut out);
        return out;
    }
    for arm in def
        .get("oneOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_arm_tokens(arm, &mut out);
    }
    out
}

/// Every kebab vocabulary reaching `--json`, keyed by the `definitions` entry
/// that owns it — condition registers and `ConflictOp` included, so a caller
/// wanting the residue filters [`ALREADY_WALKED`] out itself.
///
/// Reads every committed schema artifact rather than one: the same
/// `definitions` entry is duplicated verbatim across every verb whose schema
/// references it (`FetchOutcomeStatus` sits in both `fetch.json` and
/// `fetch-record.json`), so this folds those into one entry rather than
/// reading only the first file that happens to declare a name and missing
/// whatever a schema unique to some other verb adds under a name of its own.
fn kebab_vocabularies() -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for verb in json_schema::committed_verbs() {
        let schema = json_schema::committed_schema(&verb);
        let Some(definitions) = schema.get("definitions").and_then(Value::as_object) else {
            continue;
        };
        for (name, def) in definitions {
            let tokens = definition_tokens(def);
            if !tokens.is_empty() {
                out.entry(name.clone()).or_default().extend(tokens);
            }
        }
    }
    out
}

/// The schema `definitions` names this file already holds to the
/// disjointness property by another route: the four condition registers'
/// wire mirrors, plus `ConflictOp`, which
/// [`an_op_name_is_not_also_a_condition_name`] already checks with its own
/// arm-count and `Display`-agreement pins — a stronger shape than the generic
/// walk below has any way to give it. `RefusalKind` earns no entry here
/// because it is not a wire type; nothing in `docs/reference/schemas/`
/// declares it.
const ALREADY_WALKED: &[&str] = &[
    "ViolationOutput",
    "IssueKindOutput",
    "VcsErrorOutput",
    "SyncFailureOutput",
    "ConflictOp",
];

/// (vocabulary count, token count) the residue census reads today. Re-derived
/// directly from `docs/reference/schemas/*.json` for this file, not carried
/// over from any earlier count — an earlier count of this same residue
/// (20 vocabularies, 55 tokens) undercounted several externally-tagged doctor
/// sub-kinds and missed four vocabularies outright; this file's own walk is
/// the source of truth for the number below, and a mismatch here means the
/// walk moved, not that the constant needs editing to match.
const UNWALKED_KEBAB_CENSUS: (usize, usize) = (23, 88);

/// A residue sub-kind token deliberately mirrored from `RefusalKind`, and the
/// vocabulary that mirrors it — the D1 sentence again, one level deeper than
/// [`INTENDED_REUSE`] reaches.
///
/// `src/refusal.rs`'s own module doc states the rule these seven follow: some
/// `RefusalKind` variants "deliberately mint a token a `rwv doctor` finding
/// ... already publishes, because a token names one condition wherever it
/// appears," and each is marked "Shared with the finding" at its declaration.
/// Two of the seven go further than documentation: `MarkerDefect::kind()`
/// (`src/workspace.rs`) builds `RefusalKind::DanglingPrimary` and
/// `RefusalKind::UnreadableMarker` directly from the doctor variant, so those
/// two cannot drift apart in code even if this list went stale.
///
/// These sit outside [`INTENDED_REUSE`] rather than inside it because none of
/// them is a token any of the five hand-walked registers mints — a doctor
/// sub-kind is not part of `ViolationOutput`'s own `kind`, so `registers()`
/// never sees it, and only `RefusalKind` half of each pair does.
const INTENDED_SUBKIND_REUSE: &[(&str, &str)] = &[
    ("unmigrated-ephemeral-branch", "BranchDisciplineKind"),
    ("standalone-in-workweave", "CloneTopologyKind"),
    ("dangling-primary", "MarkerDefect"),
    ("dangling-parent", "WorkweaveTreeIntegrityKind"),
    ("stale-registry-entry", "WorkweaveTreeIntegrityKind"),
    ("unreadable-marker", "WorkweaveTreeIntegrityKind"),
    ("unregistered-workweave", "WorkweaveTreeIntegrityKind"),
];

/// Every kebab vocabulary reaching `--json` that is not one of
/// [`ALREADY_WALKED`] is disjoint from the condition universe, except the
/// seven recorded in [`INTENDED_SUBKIND_REUSE`] —
/// [`an_op_name_is_not_also_a_condition_name`]'s property, held for every
/// such vocabulary at once rather than for `ConflictOp` alone.
///
/// Population is read from the committed schema artifacts under
/// `docs/reference/schemas/`, not from source: these are outcome tags, drift
/// classifications, and lock/containment relations minted across a dozen
/// files with three different schemars shapes depending on whether a variant
/// carries a doc comment or fields, and the artifacts already normalise that
/// for every verb that publishes one. They are `generate-explain`'s own
/// output and are drift-gated by `scripts/ci-local.sh`'s final stage, so a
/// stale artifact here is a stale artifact everywhere else that reads one.
///
/// Pinned to [`UNWALKED_KEBAB_CENSUS`], an exact count rather than a floor —
/// a floor catches the walk going dark wholesale and misses it losing one
/// vocabulary, which is the more likely drift. Both directions are the alarm:
/// a shrink means a vocabulary this test relied on vanished or renamed out
/// from under the pin, a growth means a new one arrived unrecorded, and either
/// way the fix is to re-read `docs/reference/schemas/*.json`, confirm the new
/// number by hand, and move the pin — never to adjust it to make the run
/// green without reading what moved.
#[test]
fn every_unwalked_kebab_vocabulary_is_disjoint_from_conditions() {
    let vocabs = kebab_vocabularies();
    assert!(
        !vocabs.is_empty(),
        "the schema-artifact walk read no kebab vocabulary at all — \
         docs/reference/schemas/*.json parsing has gone blind, and every assertion below is \
         vacuous"
    );

    let residue: BTreeMap<&String, &BTreeSet<String>> = vocabs
        .iter()
        .filter(|(name, _)| !ALREADY_WALKED.contains(&name.as_str()))
        .collect();
    let residue_tokens: usize = residue.values().map(|v| v.len()).sum();
    assert_eq!(
        (residue.len(), residue_tokens),
        UNWALKED_KEBAB_CENSUS,
        "the unwalked kebab-vocabulary census has moved — a vocabulary arrived, left, or \
         changed size in docs/reference/schemas/*.json; re-derive the count by hand before \
         moving the pin:\n{residue:#?}"
    );

    let conditions: BTreeSet<String> = registers().values().flatten().cloned().collect();
    let collided: BTreeSet<(&str, &str)> = residue
        .iter()
        .flat_map(|(name, tokens)| tokens.iter().map(move |t| (t.as_str(), name.as_str())))
        .filter(|(token, _)| conditions.contains(*token))
        .collect();
    let recorded: BTreeSet<(&str, &str)> = INTENDED_SUBKIND_REUSE.iter().copied().collect();

    let unrecorded: Vec<&(&str, &str)> = collided.difference(&recorded).collect();
    assert!(
        unrecorded.is_empty(),
        "these residue (token, vocabulary) pairs collide with a condition name and nothing \
         records the reuse as deliberate — an operator reading one off `--json` cannot tell \
         which vocabulary it names. If this is a shared condition, record it in \
         INTENDED_SUBKIND_REUSE; if two conditions collided on one name, rename one of \
         them:\n{unrecorded:#?}"
    );

    let vanished: Vec<&(&str, &str)> = recorded.difference(&collided).collect();
    assert!(
        vanished.is_empty(),
        "these sub-kind reuses are recorded but no longer collide with a condition name — the \
         token or the vocabulary moved, which is the drift this check exists to catch:\n\
         {vanished:#?}"
    );
}
