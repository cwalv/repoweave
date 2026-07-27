//! What `rwv doctor --fix` repairs is stated in one place —
//! `CheckViolation::fix_disposition` — and every other statement of it is
//! compared against that one here.
//!
//! The regression this exists for: the set was spelled six ways, with nothing
//! comparing any two of them. The `--fix` flag help named nine arms, the
//! `rwv explain doctor` page named thirteen, `docs/reference/cli.md` named
//! five, and the code ran nineteen. Two of the doc comments on the finding
//! types were provably wrong at the same time — `CheckViolation`'s
//! `WorkweaveTreeIntegrity` said "the other three sub-kinds are report-only"
//! about an enum that had grown to eight sub-kinds of which three are
//! auto-fixable, and `BranchDisciplineKind` called its whole (a) grouping
//! report-only after one member of it became consent-gated. Every one of
//! those was green.
//!
//! Three of the six spellings were operator-facing prose that enumerated the
//! set from memory. Those were deleted rather than pinned: they now point at
//! `docs/reference/doctor-findings.md`, which is the one prose enumeration
//! left and the one this file checks. A list nobody has to maintain cannot
//! drift.
//!
//! Two instruments, because the two survivors fail differently:
//!
//!   1. **The published page disagrees with the register.** Every entry on
//!      that page opens with an Auto-fixable / Report-only / Report-only by
//!      default mark, and the mark must equal what `fix_disposition` returns
//!      for the finding it is keyed to — in both directions, so a page entry
//!      for a finding that no longer exists is reported too.
//!   2. **A doc comment on the finding type contradicts the register.** A
//!      variant doc may explain what a repair does and why one is withheld;
//!      it may not state a disposition the register disagrees with, and a
//!      variant that owns sub-kinds may not state one at all, because the
//!      disposition is per sub-kind and a claim about a set is what went
//!      stale twice.
//!
//! Both instruments take their corpus from `tests/common/doctor_corpus.rs`,
//! whose `case_token` matches exhaustively — so a finding added without a
//! sample stops this file compiling, and `fix_disposition` itself does not
//! compile until the new finding's disposition is declared.
//!
//! **Residue** — what these do *not* cover:
//!
//!   - The register is a declaration, not a recording of what the repair
//!     functions do. `apply_finding_repairs` is held to it (it cannot act on
//!     a finding the register calls report-only), but the two passes that
//!     repair workspace state before collection name no finding at all, so
//!     nothing here reads them. Adding a repair arm without declaring it
//!     makes the arm dead rather than making this file red.
//!   - Findings with no `CheckViolation` variant — surfacing symlinks,
//!     integration-content drift, member incompatibilities — carry no
//!     disposition here. The page documents them under its own heading and
//!     that section is unchecked.
//!   - A doc comment that describes a repair without naming a disposition
//!     ("`--fix` retracts it") is not read as a claim, so it cannot be
//!     reported as a wrong one.

mod common;

use common::doctor_corpus::{case_token, corpus};
use repoweave::check::FixDisposition;
use std::collections::{BTreeMap, BTreeSet};

const PAGE_PATH: &str = "docs/reference/doctor-findings.md";
const PAGE: &str = include_str!("../docs/reference/doctor-findings.md");
const CHECK_RS: &str = include_str!("../src/check.rs");

/// Token → disposition, straight off the register, for every finding the
/// corpus can build.
fn register() -> BTreeMap<String, FixDisposition> {
    corpus()
        .iter()
        .map(|v| (case_token(v), v.fix_disposition()))
        .collect()
}

fn describe(d: FixDisposition) -> String {
    match d {
        FixDisposition::Auto => "Auto-fixable".to_string(),
        FixDisposition::Consented(flag) => format!("Report-only by default ({flag})"),
        FixDisposition::ReportOnly => "Report-only".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Instrument 1: the published page against the register
// ---------------------------------------------------------------------------

/// The marks an entry may open with. Anything else in that position is
/// reported: a mark the reader has to interpret is one the check cannot.
fn mark(tag: &str) -> Option<Option<FixDisposition>> {
    Some(match tag {
        "Warning" | "Error" => None,
        "Auto-fixable" => Some(FixDisposition::Auto),
        "Report-only" => Some(FixDisposition::ReportOnly),
        // The flag is checked against the register's own, from the body.
        "Report-only by default" => Some(FixDisposition::Consented("")),
        _ => return None,
    })
}

/// One `###` or `####` heading and the prose under it.
struct Section {
    level: usize,
    token: String,
    line: usize,
    /// `None` when the section's first prose line is not a bold span.
    opening: Option<String>,
    sealed: bool,
    body: String,
}

fn sections(page: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for (i, line) in page.lines().enumerate() {
        let heading = line
            .strip_prefix("#### ")
            .map(|rest| (4, rest))
            .or_else(|| line.strip_prefix("### ").map(|rest| (3, rest)));
        if let Some((level, title)) = heading {
            let token = title
                .split('`')
                .nth(1)
                .map(str::to_string)
                .unwrap_or_default();
            out.push(Section {
                level,
                token,
                line: i + 1,
                opening: None,
                sealed: false,
                body: String::new(),
            });
            continue;
        }
        let Some(current) = out.last_mut() else {
            continue;
        };
        if !current.sealed && !line.trim().is_empty() {
            current.opening = line
                .strip_prefix("**")
                .and_then(|r| r.split("**").next())
                .map(str::to_string);
            current.sealed = true;
        }
        current.body.push_str(line);
        current.body.push('\n');
    }
    out
}

/// A finding entry on the page: its token, the mark it opens with, and the
/// prose that must name the consent flag when the mark is a consented one.
struct Entry {
    disposition: FixDisposition,
    line: usize,
    body: String,
}

/// Read the page into `token -> Entry`, reporting anything the mark
/// vocabulary does not cover.
fn read_page(page: &str) -> (BTreeMap<String, Entry>, Vec<String>) {
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let mut problems = Vec::new();
    let mut parent: Option<(String, Option<FixDisposition>)> = None;

    for section in sections(page) {
        let declared = match &section.opening {
            None => None,
            Some(span) => {
                let mut found = None;
                for tag in span.split('.').map(str::trim).filter(|t| !t.is_empty()) {
                    match mark(tag) {
                        Some(Some(d)) => found = Some(d),
                        Some(None) => {}
                        None => problems.push(format!(
                            "{PAGE_PATH}:{}: `{}` opens with `{tag}`, which is not one of the \
                             three marks — an entry whose disposition has to be interpreted \
                             cannot be compared to the code",
                            section.line, section.token
                        )),
                    }
                }
                found
            }
        };

        let key = match section.level {
            3 => {
                parent = Some((section.token.clone(), declared));
                section.token.clone()
            }
            _ => {
                let Some((kind, inherited)) = parent.clone() else {
                    problems.push(format!(
                        "{PAGE_PATH}:{}: `{}` is a sub-kind with no enclosing finding heading",
                        section.line, section.token
                    ));
                    continue;
                };
                if declared.is_none() && inherited.is_none() {
                    problems.push(format!(
                        "{PAGE_PATH}:{}: `{kind}/{}` declares no mark and its finding heading \
                         declares none to inherit",
                        section.line, section.token
                    ));
                    continue;
                }
                format!("{kind}/{}", section.token)
            }
        };

        let Some(disposition) = declared else {
            continue;
        };
        if let Some(previous) = entries.insert(
            key.clone(),
            Entry {
                disposition,
                line: section.line,
                body: section.body,
            },
        ) {
            problems.push(format!(
                "{PAGE_PATH}:{}: `{key}` is documented twice (also at line {})",
                section.line, previous.line
            ));
        }
    }
    (entries, problems)
}

/// Compare the page against the register in both directions.
fn page_gaps(page: &str, register: &BTreeMap<String, FixDisposition>) -> Vec<String> {
    let (entries, mut gaps) = read_page(page);
    let mut reached: BTreeSet<String> = BTreeSet::new();

    for (token, expected) in register {
        let kind = token.split('/').next().unwrap_or(token);
        let Some((key, entry)) = entries
            .get_key_value(token)
            .or_else(|| entries.get_key_value(kind))
        else {
            gaps.push(format!(
                "{token}: the code says {}, and {PAGE_PATH} has no entry for it",
                describe(*expected)
            ));
            continue;
        };
        reached.insert(key.clone());

        let agrees = match (entry.disposition, expected) {
            (FixDisposition::Auto, FixDisposition::Auto) => true,
            (FixDisposition::ReportOnly, FixDisposition::ReportOnly) => true,
            (FixDisposition::Consented(_), FixDisposition::Consented(flag)) => {
                if entry.body.contains(flag) {
                    true
                } else {
                    gaps.push(format!(
                        "{token}: {PAGE_PATH}:{} marks it Report-only by default without \
                         naming `{flag}`, the flag that repairs it",
                        entry.line
                    ));
                    true
                }
            }
            _ => false,
        };
        if !agrees {
            gaps.push(format!(
                "{token}: the code says {}, {PAGE_PATH}:{} says {}",
                describe(*expected),
                entry.line,
                describe(entry.disposition)
            ));
        }
    }

    for (key, entry) in &entries {
        if !reached.contains(key) {
            gaps.push(format!(
                "{PAGE_PATH}:{}: `{key}` is documented as {}, and no finding carries that \
                 token — the entry outlived the code",
                entry.line,
                describe(entry.disposition)
            ));
        }
    }
    gaps
}

#[test]
fn the_published_page_marks_every_finding_the_way_the_code_repairs_it() {
    let gaps = page_gaps(PAGE, &register());
    assert!(
        gaps.is_empty(),
        "{PAGE_PATH} and `CheckViolation::fix_disposition` disagree:\n  {}",
        gaps.join("\n  ")
    );
}

#[test]
fn the_page_walk_actually_reads_the_page() {
    let (entries, problems) = read_page(PAGE);
    assert!(
        problems.is_empty(),
        "the page walk reported constructs it could not classify:\n  {}",
        problems.join("\n  ")
    );
    assert!(
        entries.len() >= 40,
        "the page parser recovered only {} entries — a parser that reads nothing \
         agrees with every register",
        entries.len()
    );
    let register = register();
    assert!(
        register.len() >= 50,
        "the register walk yielded only {} findings",
        register.len()
    );
    let auto = register
        .values()
        .filter(|d| matches!(d, FixDisposition::Auto))
        .count();
    let consented = register
        .values()
        .filter(|d| matches!(d, FixDisposition::Consented(_)))
        .count();
    assert!(
        auto >= 15,
        "only {auto} findings are auto-fixable; the register has stopped classifying"
    );
    assert_eq!(
        consented, 2,
        "exactly two findings are repaired behind a consent flag; got {consented}"
    );
    for expected in [
        "index-drift/safe-to-fix",
        "branch-discipline/canonical-detached",
        "workweave-tree-integrity/unregistered-workweave",
        "stale-op-state",
    ] {
        assert!(
            register.contains_key(expected),
            "the corpus must carry a `{expected}` sample"
        );
    }
}

#[test]
fn the_page_check_reports_a_mark_that_drifted_from_the_code() {
    let register = register();

    let flipped = PAGE.replace(
        "#### `redundant`\n\n**Warning. Auto-fixable.**",
        "#### `redundant`\n\n**Warning. Report-only.**",
    );
    assert_ne!(flipped, PAGE, "the seeded edit matched nothing");
    let gaps = page_gaps(&flipped, &register);
    assert!(
        gaps.iter()
            .any(|g| g.starts_with("orphaned-savepoint/redundant:") && g.contains("Report-only")),
        "a mark flipped away from the code must be reported; got:\n  {}",
        gaps.join("\n  ")
    );

    let dropped = PAGE.replace("### `dangling-ref-receipt`", "### `not-a-finding`");
    assert_ne!(dropped, PAGE, "the seeded edit matched nothing");
    let gaps = page_gaps(&dropped, &register);
    assert!(
        gaps.iter().any(|g| g.starts_with("dangling-ref-receipt:")),
        "a finding the page stops documenting must be reported; got:\n  {}",
        gaps.join("\n  ")
    );
    assert!(
        gaps.iter().any(|g| g.contains("`not-a-finding`")),
        "an entry for a finding that does not exist must be reported; got:\n  {}",
        gaps.join("\n  ")
    );

    let unmarked = PAGE.replace(
        "**Warning. Report-only.** A `.rwv-op` file",
        "**Warning. Left alone.** A `.rwv-op` file",
    );
    assert_ne!(unmarked, PAGE, "the seeded edit matched nothing");
    let gaps = page_gaps(&unmarked, &register);
    assert!(
        gaps.iter().any(|g| g.contains("`Left alone`")),
        "a mark outside the vocabulary must be reported rather than skipped; got:\n  {}",
        gaps.join("\n  ")
    );

    let unflagged = PAGE.replace("rwv doctor --fix --reattach-checkouts", "rwv doctor --fix");
    assert_ne!(unflagged, PAGE, "the seeded edit matched nothing");
    let gaps = page_gaps(&unflagged, &register);
    assert!(
        gaps.iter()
            .any(|g| g.starts_with("branch-discipline/canonical-detached:")
                && g.contains("--reattach-checkouts")),
        "a consented entry that stops naming its flag must be reported; got:\n  {}",
        gaps.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// Instrument 2: the finding types' own doc comments
// ---------------------------------------------------------------------------

/// Phrases that state a disposition, longest first so a negation is consumed
/// before the word it negates.
const CLAIMS: &[(&str, FixDisposition)] = &[
    ("not auto-fixable", FixDisposition::ReportOnly),
    ("never auto-fixed", FixDisposition::ReportOnly),
    ("no auto-fix", FixDisposition::ReportOnly),
    ("safe to auto-fix", FixDisposition::Auto),
    ("auto-fixable", FixDisposition::Auto),
    ("report-only", FixDisposition::ReportOnly),
];

/// Everything in the family the vocabulary above is carved out of. A block
/// matching this and none of [`CLAIMS`] is reported: an unread spelling is
/// how a one-spelling matcher passes a contradiction.
const FAMILY: &[&str] = &["auto-fix", "report-only", "report only"];

#[derive(Debug, PartialEq)]
enum Claim {
    None,
    Stated(FixDisposition),
    Unreadable(String),
}

/// Lower-cased, with markdown emphasis dropped and every run of whitespace
/// collapsed. A claim split across two `///` lines by a `**bold**` span is the
/// same claim, and the first version of this check missed one for that reason.
fn flatten(doc: &str) -> String {
    doc.to_lowercase()
        .replace(['*', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn claim_of(doc: &str) -> Claim {
    let mut residue = flatten(doc);
    let mut stated: Option<FixDisposition> = None;
    let mut conflicting = false;
    for (phrase, disposition) in CLAIMS {
        if !residue.contains(phrase) {
            continue;
        }
        residue = residue.replace(phrase, " ");
        match stated {
            Some(previous) if previous != *disposition => conflicting = true,
            _ => stated = Some(*disposition),
        }
    }
    if conflicting {
        return Claim::Unreadable("states two dispositions at once".into());
    }
    if let Some(unread) = FAMILY.iter().find(|f| residue.contains(**f)) {
        return Claim::Unreadable(format!("`{unread}` in a phrasing the vocabulary misses"));
    }
    match stated {
        Some(d) => Claim::Stated(d),
        None => Claim::None,
    }
}

/// A variant of one of the finding enums: its name, its doc block, and the
/// sub-kind enum it delegates to when it has one.
struct Variant {
    name: String,
    line: usize,
    doc: String,
    delegates_to: Option<String>,
}

fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Variants of `enum <name>`, with the doc block that precedes each and the
/// sub-kind enum it names in a `sub_kind` or `kind` field.
///
/// Crude on purpose, and keyed to how `src/check.rs` is written: a variant
/// starts at four-space indent with an uppercase letter, and the enum ends at
/// the first line that is exactly `}`.
fn variants_of(source: &str, enum_name: &str) -> Vec<Variant> {
    let Some(offset) = source.find(&format!("pub enum {enum_name} {{")) else {
        return Vec::new();
    };
    let preceding = source[..offset].lines().count();
    let mut out = Vec::new();
    let mut doc: Vec<&str> = Vec::new();
    let mut pending: Option<Variant> = None;
    for (i, line) in source[offset..].lines().enumerate().skip(1) {
        if line == "}" {
            break;
        }
        if let Some(variant) = pending.as_mut() {
            if let Some(field) = line.trim().strip_prefix("sub_kind: ") {
                variant.delegates_to = Some(field.trim_end_matches(',').to_string());
            } else if let Some(field) = line.trim().strip_prefix("kind: ") {
                variant.delegates_to = Some(field.trim_end_matches(',').to_string());
            }
            if line == "    }," || line == "    }" {
                out.push(pending.take().expect("a variant is open"));
            }
            continue;
        }
        if let Some(text) = line
            .strip_prefix("    /// ")
            .or_else(|| (line == "    ///").then_some(""))
        {
            doc.push(text);
            continue;
        }
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if !rest.starts_with(|c: char| c.is_uppercase()) {
            doc.clear();
            continue;
        }
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            doc.clear();
            continue;
        }
        let variant = Variant {
            name,
            line: preceding + i + 1,
            doc: doc.join("\n"),
            delegates_to: None,
        };
        doc.clear();
        if rest.ends_with('{') {
            pending = Some(variant);
        } else {
            out.push(variant);
        }
    }
    out
}

/// Doc comments on the finding types that the register contradicts, or that
/// state a disposition where no single one applies.
fn doc_contradictions(source: &str, register: &BTreeMap<String, FixDisposition>) -> Vec<String> {
    let mut found = Vec::new();
    let mut read = 0usize;

    let top = variants_of(source, "CheckViolation");
    for variant in &top {
        read += 1;
        let claim = claim_of(&variant.doc);
        if let Some(sub_enum) = &variant.delegates_to {
            let subs = variants_of(source, sub_enum);
            let tokens: Vec<String> = subs
                .iter()
                .map(|sub| format!("{}/{}", kebab(&variant.name), kebab(&sub.name)))
                .collect();
            // A claim on the owner is a claim about the whole set, which is
            // the shape that went stale twice. Allowed only while the set
            // really is uniform — a sub-kind added with a different
            // disposition then lands here rather than under a wrong sentence.
            if let Claim::Stated(stated) = claim {
                let uniform =
                    !tokens.is_empty() && tokens.iter().all(|t| register.get(t) == Some(&stated));
                if !uniform {
                    found.push(format!(
                        "src/check.rs:{}: `CheckViolation::{}` says {}, and its sub-kinds do \
                         not all agree — state the disposition per sub-kind",
                        variant.line,
                        variant.name,
                        describe(stated)
                    ));
                }
            }
            if let Claim::Unreadable(why) = &claim {
                found.push(format!(
                    "src/check.rs:{}: `CheckViolation::{}` {why}",
                    variant.line, variant.name
                ));
            }
            for (sub, token) in subs.iter().zip(&tokens) {
                read += 1;
                found.extend(disagreement(
                    &format!("{sub_enum}::{}", sub.name),
                    sub.line,
                    token,
                    claim_of(&sub.doc),
                    register,
                ));
            }
            continue;
        }
        found.extend(disagreement(
            &format!("CheckViolation::{}", variant.name),
            variant.line,
            &kebab(&variant.name),
            claim,
            register,
        ));
    }

    if read < 50 {
        found.push(format!(
            "the source walk read only {read} variants of src/check.rs — it has stopped \
             finding the finding types, and every comparison above is vacuous"
        ));
    }
    found
}

fn disagreement(
    site: &str,
    line: usize,
    token: &str,
    claim: Claim,
    register: &BTreeMap<String, FixDisposition>,
) -> Vec<String> {
    let Some(expected) = register.get(token) else {
        return vec![format!(
            "src/check.rs:{line}: `{site}` maps to token `{token}`, which the register does \
             not carry — the naming convention the walk assumes has broken"
        )];
    };
    match claim {
        Claim::None => Vec::new(),
        Claim::Unreadable(why) => vec![format!(
            "src/check.rs:{line}: `{site}` {why}, so nothing can check it against the register"
        )],
        Claim::Stated(stated) => {
            let agrees = matches!(
                (stated, expected),
                (FixDisposition::Auto, FixDisposition::Auto)
                    | (FixDisposition::ReportOnly, FixDisposition::ReportOnly)
            );
            if agrees {
                Vec::new()
            } else {
                vec![format!(
                    "src/check.rs:{line}: `{site}` says {}, the register says {}",
                    describe(stated),
                    describe(*expected)
                )]
            }
        }
    }
}

#[test]
fn no_finding_doc_comment_contradicts_the_register() {
    let found = doc_contradictions(CHECK_RS, &register());
    assert!(
        found.is_empty(),
        "doc comments on the finding types disagree with \
         `CheckViolation::fix_disposition`:\n  {}",
        found.join("\n  ")
    );
}

/// The vacuity this instrument is one broken `flatten` away from: a
/// classifier that reads every block as making no claim reports nothing and
/// is indistinguishable, when green, from a tree with no contradictions.
#[test]
fn the_doc_walk_actually_recognises_the_claims_in_the_source() {
    let top = variants_of(CHECK_RS, "CheckViolation");
    assert!(
        top.len() >= 30,
        "the variant parser recovered only {} `CheckViolation` variants",
        top.len()
    );
    let owners = top.iter().filter(|v| v.delegates_to.is_some()).count();
    assert!(
        owners >= 8,
        "only {owners} variants were read as owning a sub-kind enum, so most of the \
         sub-kind docs are never visited"
    );
    let stated = top
        .iter()
        .filter(|v| matches!(claim_of(&v.doc), Claim::Stated(_)))
        .count();
    assert!(
        stated >= 10,
        "the classifier recognised a disposition in only {stated} of the {} \
         `CheckViolation` docs — it has stopped reading claims, and every comparison \
         above passes for that reason",
        top.len()
    );
    // The two spellings that broke the first version of the classifier: one
    // split across `///` lines by a bold span, one that says "safe to
    // auto-fix" rather than "auto-fixable".
    assert_eq!(
        claim_of("age and the path. **Never\nauto-fixed**: another terminal may be"),
        Claim::Stated(FixDisposition::ReportOnly)
    );
    assert_eq!(
        claim_of("Safe to auto-fix with `git reset`"),
        Claim::Stated(FixDisposition::Auto)
    );
    assert_eq!(claim_of("`--fix` retracts it"), Claim::None);
}

#[test]
fn the_doc_check_reports_a_comment_that_drifted_from_the_register() {
    let register = register();

    let flipped = CHECK_RS.replace(
        "    /// The savepoint tip is **not** reachable",
        "    /// Auto-fixable. The savepoint tip is **not** reachable",
    );
    assert_ne!(flipped, CHECK_RS, "the seeded edit matched nothing");
    let found = doc_contradictions(&flipped, &register);
    assert!(
        found
            .iter()
            .any(|f| f.contains("OrphanedSavepointKind::Live") && f.contains("Auto-fixable")),
        "a doc claiming a repair the register withholds must be reported; got:\n  {}",
        found.join("\n  ")
    );

    let on_parent = CHECK_RS.replace(
        "    /// A `.rwv-workweave` marker tree anomaly:",
        "    /// Auto-fixable. A `.rwv-workweave` marker tree anomaly:",
    );
    assert_ne!(on_parent, CHECK_RS, "the seeded edit matched nothing");
    let found = doc_contradictions(&on_parent, &register);
    assert!(
        found
            .iter()
            .any(|f| f.contains("CheckViolation::WorkweaveTreeIntegrity")),
        "the shape that went stale twice — one claim covering a set of sub-kinds — must be \
         reported; got:\n  {}",
        found.join("\n  ")
    );

    let unread = CHECK_RS.replace(
        "    /// A directory under a registry path not listed",
        "    /// Beyond auto-fixing. A directory under a registry path not listed",
    );
    assert_ne!(unread, CHECK_RS, "the seeded edit matched nothing");
    let found = doc_contradictions(&unread, &register);
    assert!(
        found
            .iter()
            .any(|f| f.contains("CheckViolation::OrphanedClone") && f.contains("vocabulary")),
        "a disposition phrased outside the vocabulary must be reported, not skipped; got:\n  {}",
        found.join("\n  ")
    );

    let blinded = doc_contradictions("pub enum CheckViolation {\n}\n", &register);
    assert!(
        blinded.iter().any(|f| f.contains("vacuous")),
        "a walk that reads nothing must say so; got:\n  {}",
        blinded.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// Instrument 3: the operator surfaces point at the page instead of copying it
// ---------------------------------------------------------------------------

#[test]
fn the_fix_flag_help_points_at_the_page_and_names_no_arms() {
    let help = String::from_utf8(
        common::rwv()
            .args(["doctor", "--help"])
            .output()
            .expect("`rwv doctor --help` runs")
            .stdout,
    )
    .expect("help output is UTF-8");

    assert!(
        help.contains("doctor-findings.md"),
        "`--fix` help must send the operator to the one enumeration; got:\n{help}"
    );
    for (token, disposition) in register() {
        if matches!(disposition, FixDisposition::Auto) {
            assert!(
                !help.contains(&token),
                "`rwv doctor --help` names the finding `{token}` — the flag help is \
                 enumerating the set again"
            );
        }
    }
    for flag in register().values().filter_map(|d| match d {
        FixDisposition::Consented(flag) => Some(*flag),
        _ => None,
    }) {
        assert!(
            help.contains(flag),
            "the register names `{flag}` as a consent flag and `rwv doctor --help` does not \
             carry it"
        );
    }
}

#[test]
fn the_explain_page_points_at_the_findings_page() {
    let template = include_str!("../docs/reference/explain/templates/doctor.md.tmpl");
    assert!(
        template.contains("docs/reference/doctor-findings.md"),
        "`rwv explain doctor` must send the operator to the one enumeration"
    );
    let cli_md = include_str!("../docs/reference/cli.md");
    assert!(
        cli_md.contains("doctor-findings.md"),
        "the `--fix` row of docs/reference/cli.md must send the operator to the one \
         enumeration"
    );
}
