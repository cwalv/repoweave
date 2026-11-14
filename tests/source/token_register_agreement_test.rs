//! One token string, one condition, across every register that publishes one.
//!
//! Six vocabularies name a condition to an operator as a stable kebab-case
//! token: `RefusalKind`, the `Axis` kinds a judged refusal routes to,
//! `CheckViolation`'s doctor kinds, `IssueKind`'s tags, `VcsErrorOutput`'s
//! wire kinds, and `SyncFailureOutput`'s. A token appearing
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
//! Structural, under docs/internals/testing.md's license 2 — a prohibition over an enumerable
//! population. The populations are read out of the source that declares them,
//! never listed here; the one thing this file states is *intent*, which is the
//! one thing source cannot tell you (see [`INTENDED_REUSE`]).
//!
//! **Scope.** The six registers above. `RefusalKind`'s and `Axis`'s tokens are
//! read from the schemas schemars derives for them; `IssueKind`'s tags are what
//! `IssueKindOutput` serializes, read from its committed schema; the doctor,
//! VCS and sync kinds are `ViolationKind::ALL`, `VcsErrorKind::ALL` and
//! `SyncFailureKind::ALL`, the types whose serialization is the tag.
//!
//! **A field value is not a condition.** `ConflictOp` serializes `rebase` /
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
//! six `RefusalKind` conditions — the same D1 sentence one level below the
//! five registers, recorded in [`INTENDED_SUBKIND_REUSE`] rather than
//! [`INTENDED_REUSE`] because a sub-kind is not a token any of the five
//! registers mints.

use crate::common;

use common::json_schema;
use repoweave::check::ViolationKind;
use repoweave::diagnosis::Axis;
use repoweave::refusal::RefusalKind;
use repoweave::sync::SyncFailureKind;
use repoweave::vcs::VcsErrorKind;
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
        "alignment",
        &["doctor", "kind"],
        "a checkout is not where its workspace expects it",
    ),
    (
        "history-disagreement",
        &["doctor", "kind"],
        "projects record different `history` for one member",
    ),
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
        "unreadable-active-project",
        &["doctor", "refusal"],
        "the active-project pointer holds no project name",
    ),
    (
        "unreadable-workweave-index",
        &["doctor", "refusal"],
        "a project's workweave index does not parse",
    ),
    (
        "unreadable-op-record",
        &["doctor", "refusal"],
        "an operation record this rwv cannot read",
    ),
    (
        "dead-op-lease",
        &["doctor", "refusal"],
        "an op-state lease outlived the process that took it",
    ),
    (
        "claim-owner-unreachable",
        &["doctor", "refusal"],
        "a landing claim's owner workspace is not here to read",
    ),
    (
        "claim-format-unknown",
        &["doctor", "refusal"],
        "a landing claim or its takeover guard was written by a different rwv version",
    ),
    (
        "claim-unopenable",
        &["doctor", "refusal"],
        "a landing claim or its takeover guard cannot be read at all",
    ),
    (
        "claim-unreadable",
        &["doctor", "refusal"],
        "a landing claim or its takeover guard holds what this rwv does not read",
    ),
    (
        "head-unreadable",
        &["doctor", "sync"],
        "a repo's HEAD could not be read",
    ),
    (
        "legacy-manifest-format",
        &["doctor", "refusal"],
        "the project carries a manifest in the retired format",
    ),
    (
        "legacy-workweave-index",
        &["doctor", "kind"],
        "the workweave index is at the path an earlier release kept it",
    ),
    (
        "legacy-workweave-marker",
        &["doctor", "refusal"],
        "the workweave marker is in the retired format",
    ),
    (
        "missing-canonical-clone",
        &["doctor", "refusal"],
        "the canonical clone a checkout derives from is gone",
    ),
    (
        "missing-lock-merge-driver",
        &["doctor", "kind"],
        "the project repo's lock merge driver is not set up as this release sets it up",
    ),
    (
        "pin",
        &["doctor", "kind"],
        "a member's checkout does not stand at the commit its lock pins, or the lock pins none",
    ),
    (
        "unparseable-project",
        &["doctor", "kind"],
        "a project directory does not parse as a project",
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
    (
        "workweave-drift",
        &["doctor", "refusal"],
        "a workweave lacks the worktree a member needs",
    ),
];

fn source(file: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file))
        .unwrap_or_else(|e| panic!("src/{file} is readable: {e}"))
}

/// The tokens a `rename_all = "kebab-case"` enum publishes: its variant names
/// kebabbed, **except** where an explicit `#[serde(rename = "…")]` overrides
/// one.
///
/// Honoring the override is not a nicety. A variant attribute is how a token
/// is renamed without touching Rust code, so a parser that reads only variant
/// names is blind to the exact edit this file exists to catch — three
/// mutations proved it by slipping past an earlier version of this function.
///
/// Fieldless variants only, which is what its caller declares. A variant
/// carrying fields spans lines this yields nothing for, so an enum that grows
/// one goes partially blind.
fn serde_enum_tokens(src: &str, decl: &str) -> BTreeSet<String> {
    let body = src
        .split_once(decl)
        .unwrap_or_else(|| panic!("`{decl}` is declared"))
        .1;
    let body = body.split_once("\n}").expect("the enum closes").0;
    let mut out = BTreeSet::new();
    let mut override_token: Option<String> = None;
    for line in body.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
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
    json_schema::derived_enum_tokens::<RefusalKind>()
}

/// The kinds a judged refusal routes to: the axes of the state-to-choices
/// table, read from the schema derived for `Axis`.
fn kind_tokens() -> BTreeSet<String> {
    json_schema::derived_enum_tokens::<Axis>()
}

/// The tags an integration finding's kind carries, read from the committed
/// schema of the type that serializes them. `IssueKind::tag` is that
/// serialization, so there is no second spelling to read.
fn issue_tokens() -> BTreeSet<String> {
    kebab_vocabularies()
        .remove("IssueKindOutput")
        .expect("the doctor schema declares IssueKindOutput")
}

/// The op names `ConflictOp` puts on the wire, read from the declaration that
/// serializes them.
fn conflict_op_tokens() -> BTreeSet<String> {
    serde_enum_tokens(&source("vcs.rs"), "pub enum ConflictOp {")
}

/// The declarations [`serde_enum_tokens`] is pointed at, as the file each sits
/// in and the line that opens it.
///
/// A call site outside this list is not covered by
/// [`the_declaration_walk_reads_the_same_tokens_on_a_crlf_checkout`]. An entry
/// that goes stale cannot pass quietly — `serde_enum_tokens` panics on a
/// declaration it cannot find.
const SERDE_ENUM_DECLARATIONS: &[(&str, &str)] = &[
    ("vcs.rs", "pub enum ConflictOp {"),
    ("refusal.rs", "pub enum RefusalKind {"),
];

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

fn doctor_tokens() -> BTreeSet<String> {
    ViolationKind::ALL.iter().map(ToString::to_string).collect()
}

fn vcs_tokens() -> BTreeSet<String> {
    VcsErrorKind::ALL.iter().map(ToString::to_string).collect()
}

fn sync_failure_tokens() -> BTreeSet<String> {
    SyncFailureKind::ALL
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn registers() -> BTreeMap<&'static str, BTreeSet<String>> {
    BTreeMap::from([
        ("refusal", refusal_tokens()),
        ("kind", kind_tokens()),
        ("issue", issue_tokens()),
        ("vcs", vcs_tokens()),
        ("sync", sync_failure_tokens()),
        ("doctor", doctor_tokens()),
    ])
}

/// Every register is read: each holds a token it is known to hold, and the
/// two read from a derived schema agree with the enum they derive from. A
/// parser that quietly stopped matching would make every assertion below
/// vacuous while leaving them green. The registers read from a type's `ALL`
/// are held to their wire mirrors' schemas by
/// [`the_kind_enums_list_exactly_the_tags_their_wire_mirrors_declare`].
#[test]
fn the_register_walk_is_not_vacuous() {
    let regs = registers();
    for (name, witness) in [
        ("refusal", "op-in-progress"),
        ("kind", "edits"),
        ("doctor", "orphaned-clone"),
        ("issue", "tool-missing"),
        ("vcs", "not-a-repo"),
        ("sync", "merge-failed"),
    ] {
        assert!(
            regs[name].contains(witness),
            "the {name} register walk did not read `{witness}`; it has stopped reading that \
             register: {:?}",
            regs[name]
        );
    }
    assert_eq!(
        regs["refusal"],
        serde_enum_tokens(&source("refusal.rs"), "pub enum RefusalKind {"),
        "the schema derived for RefusalKind and its declaration name different tokens"
    );
    assert_eq!(
        regs["kind"],
        Axis::ALL.iter().map(|a| a.token()).collect::<BTreeSet<_>>(),
        "the schema derived for Axis and Axis::ALL name different kinds"
    );
}

/// [`serde_enum_tokens`] reads a declaration the same way whatever a checkout
/// spells its line endings, which is what keeps the last variant of every
/// register it walks from vanishing.
///
/// `str::lines()` drops a trailing `\r` only from a segment that ends in `\n`.
/// Slicing the body at `"\n}"` leaves the final variant as a segment with no
/// terminator, so on a CRLF checkout that one alone arrives wearing a bare
/// `\r` and falls out of every prefix and suffix match — one variant short, in
/// silence, under floors that sit well below either register's size.
///
/// Driven from the declarations themselves rather than a hand-written enum, so
/// a register spelled in a way a fixture never anticipated is still covered.
#[test]
fn the_declaration_walk_reads_the_same_tokens_on_a_crlf_checkout() {
    for (file, decl) in SERDE_ENUM_DECLARATIONS {
        let lf = source(file).replace("\r\n", "\n");
        let crlf = lf.replace('\n', "\r\n");
        let native = serde_enum_tokens(&lf, decl);
        assert!(
            !native.is_empty(),
            "src/{file}'s `{decl}` walk yielded nothing on an LF checkout, so the comparison \
             below would hold over two empty sets"
        );
        let converted = serde_enum_tokens(&crlf, decl);
        assert_eq!(
            converted,
            native,
            "src/{file}'s `{decl}` walk recovers {} tokens on a CRLF checkout against {} on an \
             LF one — a line-ending assumption is dropping variants, the declaration's last \
             one first:\n{:#?}",
            converted.len(),
            native.len(),
            native.difference(&converted).collect::<Vec<_>>()
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
/// The committed schema is what keeps this from passing vacuously. A parser
/// that stopped reading the declaration would assert disjointness over nothing,
/// and the schema is generated from the type, so it is a second reading the
/// walk must agree with. `Display` is not read: it is the serialized token by
/// construction, and `tests/source/display_is_wire_token_test.rs` holds it there.
#[test]
fn an_op_name_is_not_also_a_condition_name() {
    let ops = conflict_op_tokens();
    let declared = kebab_vocabularies()
        .remove("ConflictOp")
        .unwrap_or_default();
    assert!(
        !declared.is_empty(),
        "ConflictOp's schema yielded no tokens"
    );
    assert_eq!(
        ops, declared,
        "ConflictOp's declaration walk and its committed schema disagree: the walk has gone \
         blind, or the artifact is stale"
    );

    let conditions: BTreeSet<String> = registers().values().flatten().cloned().collect();
    let collided: Vec<&String> = ops.iter().filter(|op| conditions.contains(*op)).collect();
    assert!(
        collided.is_empty(),
        "these name an in-flight operation on one surface and a condition on another, and \
         an operator reading one off `--json` cannot tell which:\n{collided:#?}"
    );
}

/// The kind enums list exactly the tags their wire mirrors declare. The mirrors
/// are what `--json` serializes and what the committed schemas are generated
/// from, so a variant added to one side and not the other, or renamed on one,
/// reds here.
#[test]
fn the_kind_enums_list_exactly_the_tags_their_wire_mirrors_declare() {
    let mut vocabs = kebab_vocabularies();
    for (kinds, mirror) in [
        (doctor_tokens(), "ViolationOutput"),
        (vcs_tokens(), "VcsErrorOutput"),
        (sync_failure_tokens(), "SyncFailureOutput"),
    ] {
        let declared = vocabs
            .remove(mirror)
            .unwrap_or_else(|| panic!("the committed schemas declare {mirror}"));
        assert!(
            !declared.is_empty(),
            "{mirror}: the schema walk read nothing"
        );
        assert_eq!(kinds, declared, "{mirror}");
    }
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
    for witness in ["op-in-progress", "orphaned-clone", "command-failed"] {
        assert!(
            served.contains(witness),
            "the entry walk did not read `{witness}`; it has stopped reading the pages"
        );
    }

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
/// letters, digits and `-` only. What this excludes is a literal underscore
/// surviving into a token.
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
/// (`MemberOutcomeStatus`); a single-key `{"enum": [x]}` for one documented
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
/// references it (`PlacementOutput` sits in `fetch.json`, `update.json`,
/// `sync.json` and `land.json`), so this folds those into one entry rather than
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
/// disjointness property by another route: the condition registers' wire
/// mirrors, plus `ConflictOp`, which
/// [`an_op_name_is_not_also_a_condition_name`] already checks, reading its
/// source declaration against the schema — a stronger shape than the generic
/// walk below has any way to give it. `RefusalKind` earns no entry here
/// because it is not a wire type; nothing in `docs/reference/schemas/`
/// declares it.
const ALREADY_WALKED: &[&str] = &[
    "ViolationOutput",
    "IssueKindOutput",
    "VcsErrorOutput",
    "SyncFailureOutput",
    "Axis",
    "ConflictOp",
];

/// Every kebab token a definition holds, read without regard to how schemars
/// shaped it: each string member of an `enum` at any depth, and the one
/// required key of any `oneOf` arm whose sole property is not a string
/// literal. A second derivation of [`kebab_vocabularies`]'s population, so a
/// shape that walk stops reading shows as a difference between the two.
fn shape_blind_tokens(node: &Value, out: &mut BTreeSet<String>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value) {
                    ("enum", Value::Array(members)) => out.extend(
                        members
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|s| is_kebab_token(s))
                            .map(str::to_owned),
                    ),
                    ("oneOf", Value::Array(arms)) => {
                        for arm in arms {
                            let props = arm.get("properties").and_then(Value::as_object);
                            let required = arm.get("required").and_then(Value::as_array);
                            if let (Some(props), Some([Value::String(tag)])) =
                                (props, required.map(Vec::as_slice))
                            {
                                let payload = props.get(tag);
                                if props.len() == 1
                                    && is_kebab_token(tag)
                                    && payload.is_some_and(|p| p.get("enum").is_none())
                                {
                                    out.insert(tag.clone());
                                }
                            }
                            shape_blind_tokens(arm, out);
                        }
                    }
                    _ => shape_blind_tokens(value, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| shape_blind_tokens(i, out)),
        _ => {}
    }
}

fn shape_blind_vocabularies() -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for verb in json_schema::committed_verbs() {
        let schema = json_schema::committed_schema(&verb);
        let Some(definitions) = schema.get("definitions").and_then(Value::as_object) else {
            continue;
        };
        for (name, def) in definitions {
            let mut tokens = BTreeSet::new();
            shape_blind_tokens(def, &mut tokens);
            if !tokens.is_empty() {
                out.entry(name.clone()).or_default().extend(tokens);
            }
        }
    }
    out
}

#[test]
fn the_vocabulary_walk_reads_every_shape_the_schemas_hold() {
    let walked = kebab_vocabularies();
    assert_eq!(
        walked,
        shape_blind_vocabularies(),
        "the shape-aware walk and the shape-blind scan of docs/reference/schemas/*.json \
         disagree: a vocabulary arrived in a shape collect_arm_tokens does not read"
    );
    for (vocabulary, token) in [
        ("MemberOutcomeStatus", "skipped"),
        ("PushOutcomeOutput", "records-published"),
        ("WorkweaveTreeIntegrityKind", "dangling-parent"),
    ] {
        assert!(
            walked.get(vocabulary).is_some_and(|t| t.contains(token)),
            "the walk no longer reads `{token}` in `{vocabulary}`"
        );
    }
}

/// A residue sub-kind token deliberately mirrored from `RefusalKind`, and the
/// vocabulary that mirrors it — the D1 sentence again, one level deeper than
/// [`INTENDED_REUSE`] reaches.
///
/// `src/refusal.rs`'s own module doc states the rule these six follow: some
/// `RefusalKind` variants "deliberately mint a token a `rwv doctor` finding
/// ... already publishes, because a token names one condition wherever it
/// appears," and each is marked "Shared with the finding" at its declaration.
/// Two of the six go further than documentation: `MarkerDefect::kind()`
/// (`src/workspace.rs`) builds `RefusalKind::DanglingPrimary` and
/// `RefusalKind::UnreadableMarker` directly from the doctor variant, so those
/// two cannot drift apart in code even if this list went stale.
///
/// These sit outside [`INTENDED_REUSE`] rather than inside it because none of
/// them is a token any of the five hand-walked registers mints — a doctor
/// sub-kind is not part of `ViolationOutput`'s own `kind`, so `registers()`
/// never sees it, and only `RefusalKind` half of each pair does.
///
/// Two pairs mirror an integration issue kind instead. `SkipReason`'s
/// `tool-missing` is the reason an install hook was skipped, and that reason
/// is the `tool-missing` finding — one condition, read off `rwv update --json`
/// or `rwv fetch --json` as off `rwv doctor --json`. `AdvisoryKindOutput`'s
/// `derived-state-stale` is the advisory `rwv sync --json` emits for the
/// condition `rwv doctor` reports as the `derived-state-stale` finding.
///
/// One pair mirrors a kind. `FactKind`'s `checkout` is the fact a verb that
/// aligns members reports where the `checkout` kind's verbs refuse: a member's
/// branch checked out elsewhere, met by a verb that detaches the member rather
/// than refusing it.
const INTENDED_SUBKIND_REUSE: &[(&str, &str)] = &[
    ("checkout", "FactKind"),
    ("tool-missing", "SkipReason"),
    ("derived-state-stale", "AdvisoryKindOutput"),
    ("standalone-in-workweave", "CloneTopologyKind"),
    ("dangling-primary", "MarkerDefect"),
    ("stale-registry-entry", "WorkweaveTreeIntegrityKind"),
    ("unreadable-marker", "WorkweaveTreeIntegrityKind"),
    ("unregistered-workweave", "WorkweaveTreeIntegrityKind"),
];

/// Every kebab vocabulary reaching `--json` that is not one of
/// [`ALREADY_WALKED`] is disjoint from the condition universe, except the
/// pairs recorded in [`INTENDED_SUBKIND_REUSE`] —
/// [`an_op_name_is_not_also_a_condition_name`]'s property, held for every
/// such vocabulary at once rather than for `ConflictOp` alone.
///
/// Population is read from the committed schema artifacts under
/// `docs/reference/schemas/`, not from source: these are outcome tags, drift
/// classifications, and lock/containment relations minted across a dozen
/// files with three different schemars shapes depending on whether a variant
/// carries a doc comment or fields, and the artifacts already normalize that
/// for every verb that publishes one. They are `generate-explain`'s own
/// output and are drift-gated by `scripts/ci-local.sh`'s final stage, so a
/// stale artifact here is a stale artifact everywhere else that reads one.
///
/// That the walk reads every vocabulary is
/// [`the_vocabulary_walk_reads_every_shape_the_schemas_hold`]'s to hold.
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
