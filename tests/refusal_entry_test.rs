//! Every refusal token and every kind reaches an entry, and `rwv explain
//! <token>` serves that entry's own bytes.
//!
//! The property that makes this worth pinning is not "the page exists" but
//! that there is ONE text: a token's entry lives on exactly one published
//! page, and what the terminal prints is a slice of that page rather than a
//! copy of it. Two spellings is the failure being guarded, and a copy is how
//! it arrives. A kind's entry is generated from the state-to-choices table, so
//! the page carrying exactly the generated text is pinned here as well. A kind
//! that `rwv doctor` also reports keeps its one entry on the findings page,
//! like a token that doctor also reports.

use repoweave::diagnosis::Axis;
use repoweave::refusal::RefusalKind;
use std::collections::{BTreeMap, BTreeSet};

mod common;

use common::json_schema;

/// Every token the register mints, from the schema derived for `RefusalKind`.
fn minted_tokens() -> Vec<String> {
    json_schema::derived_enum_tokens::<RefusalKind>()
        .into_iter()
        .collect()
}

/// Every kind a judged refusal routes to, from the schema derived for `Axis`.
fn kind_tokens() -> Vec<String> {
    json_schema::derived_enum_tokens::<Axis>()
        .into_iter()
        .collect()
}

/// The published entry pages, read from disk rather than through the module
/// that serves them.
fn pages_text() -> String {
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference");
    let refusals = std::fs::read_to_string(docs.join("refusals.md")).expect("refusals published");
    let findings =
        std::fs::read_to_string(docs.join("doctor-findings.md")).expect("findings published");
    format!("{refusals}\n{findings}")
}

#[test]
fn every_minted_token_has_an_entry() {
    let tokens = minted_tokens();
    assert!(
        tokens
            .iter()
            .any(|t| *t == RefusalKind::OpInProgress.token()),
        "the register walk did not read `op-in-progress`: {tokens:?}"
    );

    let missing: Vec<&String> = tokens
        .iter()
        .filter(|t| repoweave::explain::entry_for_token(t).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "these tokens are printed to operators with no entry to route them to:\n{missing:#?}"
    );
}

/// The reverse direction: an entry whose token no longer exists is a page
/// section nothing can reach, and it outlives the code silently.
#[test]
fn every_entry_names_a_minted_token() {
    let minted: std::collections::BTreeSet<String> =
        minted_tokens().into_iter().chain(kind_tokens()).collect();
    let documented = repoweave::explain::documented_tokens();
    for witness in ["op-in-progress", "orphaned-clone"] {
        assert!(
            documented.contains(&witness),
            "the page walk did not read `{witness}`; it has stopped reading a page"
        );
    }

    // Doctor findings and issue kinds are documented on the same page and are
    // not refusal tokens; the register is the subset this direction is about.
    let refusals_page = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/refusals.md"),
    )
    .expect("the refusals page is published");
    let orphaned: Vec<&str> = refusals_page
        .lines()
        .filter_map(|l| l.strip_prefix("### `")?.strip_suffix('`'))
        .filter(|t| !minted.contains(*t))
        .collect();
    assert!(
        orphaned.is_empty(),
        "these entries outlived the tokens they document:\n{orphaned:#?}"
    );
}

/// The entry pages, read from disk rather than through the module that serves
/// them, by file name.
fn entry_pages() -> Vec<(&'static str, String)> {
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference");
    ["refusals.md", "doctor-findings.md", "vcs-errors.md"]
        .into_iter()
        .map(|name| {
            let text = std::fs::read_to_string(docs.join(name))
                .unwrap_or_else(|e| panic!("{name} is published: {e}"));
            (name, text)
        })
        .collect()
}

/// Every page a token has an entry heading on, once per heading.
fn entry_locations(pages: &[(&'static str, String)]) -> BTreeMap<String, Vec<&'static str>> {
    let mut out: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    for (name, text) in pages {
        for line in text.lines() {
            let hashes = line.len() - line.trim_start_matches('#').len();
            let token = line[hashes..]
                .trim()
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'));
            if let Some(token) = token.filter(|t| hashes >= 2 && !t.is_empty() && !t.contains(' '))
            {
                out.entry(token.to_owned()).or_default().push(name);
            }
        }
    }
    out
}

/// The tags a schemars enum definition publishes, whether as string values or
/// as externally tagged single keys.
fn schema_tags(def: &serde_json::Value) -> Vec<String> {
    let variants: Vec<&serde_json::Value> = match def["oneOf"].as_array() {
        Some(variants) => variants.iter().collect(),
        None => vec![def],
    };
    variants
        .into_iter()
        .flat_map(|v| match v["enum"].as_array() {
            Some(values) => values.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>(),
            None => v["required"]
                .as_array()
                .filter(|keys| keys.len() == 1)
                .map(|keys| keys.iter().filter_map(|k| k.as_str()).collect())
                .unwrap_or_default(),
        })
        .map(str::to_owned)
        .collect()
}

/// Every token `rwv doctor --json` publishes as a finding — a violation
/// `kind`, a `sub_kind`, or an integration issue kind — with how many
/// conditions it names: one, except a sub-kind several kinds share, which
/// names one condition under each. Read from the doctor schema schemars
/// derives, so a token the code adds is in it.
fn finding_tokens() -> BTreeMap<String, usize> {
    let json = serde_json::to_value(schemars::schema_for!(repoweave::check::DoctorJsonOutput))
        .expect("doctor schema serializes");
    let defs = &json["definitions"];
    let mut tokens: BTreeMap<String, usize> = schema_tags(&defs["IssueKindOutput"])
        .into_iter()
        .map(|t| (t, 1))
        .collect();
    for variant in defs["ViolationOutput"]["oneOf"]
        .as_array()
        .expect("violations are a oneOf")
    {
        for kind in schema_tags(&variant["properties"]["kind"]) {
            tokens.entry(kind).or_insert(1);
        }
        let sub_kind = &variant["properties"]["sub_kind"];
        if let Some(reference) = sub_kind["$ref"]
            .as_str()
            .or(sub_kind["allOf"][0]["$ref"].as_str())
        {
            let name = reference
                .rsplit('/')
                .next()
                .expect("a $ref names a definition");
            for sub in schema_tags(&defs[name]) {
                *tokens.entry(sub).or_insert(0) += 1;
            }
        }
    }
    tokens
}

/// Tokens and kinds a refusal shares with a `rwv doctor` finding — one
/// condition, reported by both.
fn tokens_shared_with_a_finding() -> BTreeSet<String> {
    let findings = finding_tokens();
    minted_tokens()
        .into_iter()
        .chain(kind_tokens())
        .filter(|t| findings.contains_key(t))
        .collect()
}

/// Tokens with more entry headings across the pages than conditions they
/// name.
fn duplicated_entries(pages: &[(&'static str, String)]) -> Vec<String> {
    let conditions = finding_tokens();
    entry_locations(pages)
        .into_iter()
        .filter(|(token, at)| at.len() > conditions.get(token).copied().unwrap_or(1))
        .map(|(token, at)| format!("`{token}` has entries on {at:?}"))
        .collect()
}

/// Shared tokens whose entries are anything but one, on the findings page.
fn misplaced_shared_entries(
    pages: &[(&'static str, String)],
    shared: &BTreeSet<String>,
) -> Vec<String> {
    let at = entry_locations(pages);
    shared
        .iter()
        .filter_map(|token| {
            let pages = at.get(token).cloned().unwrap_or_default();
            (pages != ["doctor-findings.md"]).then(|| format!("`{token}` has entries on {pages:?}"))
        })
        .collect()
}

/// A token has one entry per condition it names — one, but for a sub-kind
/// several kinds share. A second entry is not merely redundant: `rwv explain`
/// refuses a bare name that heads two entries, so it takes the token's one
/// entry out of reach of the name every refusal prints.
#[test]
fn every_token_has_one_entry_per_condition_across_the_pages() {
    let pages = entry_pages();
    let documented = entry_locations(&pages);
    for (page, witness) in [
        ("refusals.md", "op-in-progress"),
        ("doctor-findings.md", "orphaned-clone"),
        ("vcs-errors.md", "command-failed"),
    ] {
        assert_eq!(
            documented.get(witness).map(Vec::as_slice),
            Some(&[page][..]),
            "the page walk did not read `{witness}` on {page}"
        );
    }
    let duplicated = duplicated_entries(&pages);
    assert!(
        duplicated.is_empty(),
        "these tokens have more entries than conditions, which is two spellings of one \
         rule:\n{duplicated:#?}"
    );
}

/// A refusal whose condition `rwv doctor` also reports keeps its single entry
/// on the findings page, and `rwv explain` serves it from there. Serving must
/// not depend on which page carries the entry — that indifference is what
/// stops a second one being written on the refusals page. A kind of the
/// state-to-choices table is shared in the same way as a token is.
#[test]
fn a_token_shared_with_a_finding_keeps_its_one_entry_on_the_findings_page() {
    let shared = tokens_shared_with_a_finding();
    assert!(
        shared.contains("dangling-active-project")
            && shared.contains("pin")
            && shared.contains("unparseable-project"),
        "the shared set lost a token it is known to hold: {shared:?}"
    );
    let pages = entry_pages();
    let misplaced = misplaced_shared_entries(&pages, &shared);
    assert!(misplaced.is_empty(), "{misplaced:#?}");

    let findings_page = &pages
        .iter()
        .find(|(name, _)| *name == "doctor-findings.md")
        .expect("the findings page is read")
        .1;
    for token in &shared {
        let entry = repoweave::explain::entry_for_token(token)
            .unwrap_or_else(|| panic!("`{token}` resolves"));
        assert!(
            findings_page.contains(entry),
            "`rwv explain {token}` serves an entry that is not the findings page's:\n{entry}"
        );
    }
}

/// The seeded failures for the two pins above: a second entry for a shared
/// token, for a shared kind of the table and for a shared sub-kind, each
/// written on the refusals page.
#[test]
fn a_seeded_duplicate_of_a_shared_entry_is_reported() {
    let shared = tokens_shared_with_a_finding();
    for (seed, heading) in [
        ("dangling-active-project", "###"),
        ("pin", "###"),
        ("unparseable-project", "###"),
        ("dangling-parent", "####"),
    ] {
        assert!(shared.contains(seed), "`{seed}` is no longer shared");
        let mut pages = entry_pages();
        pages[0]
            .1
            .push_str(&format!("\n{heading} `{seed}`\n\nA second entry.\n"));
        let want = format!("`{seed}` has entries on [\"refusals.md\", \"doctor-findings.md\"]");
        assert_eq!(duplicated_entries(&pages), std::slice::from_ref(&want));
        assert_eq!(misplaced_shared_entries(&pages, &shared), [want]);
    }
}

/// What `rwv explain <token>` prints is the page's own bytes.
///
/// Driven through the binary rather than the library: the claim is about what
/// an operator reads, and a library-only assertion would hold even if dispatch
/// never reached the entry.
#[test]
fn explain_serves_the_page_section_verbatim() {
    // The last entry on a page is the one case with no following heading to
    // stop at, so it takes a different branch of the slicer. Derived rather
    // than named: an entry appended after it would otherwise silently take
    // that coverage away.
    let refusals_page = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/refusals.md"),
    )
    .expect("the refusals page is published");
    let last_entry = refusals_page
        .lines()
        .filter_map(|l| l.strip_prefix("### `")?.strip_suffix('`'))
        .next_back()
        .expect("the page has entries")
        .to_owned();

    for token in [
        "head-not-on-branch",
        "stale-lock",
        "dangling-parent",
        "edits",
        &last_entry,
    ] {
        let out = common::rwv()
            .args(["explain", token])
            .output()
            .expect("rwv should run");
        assert!(out.status.success(), "rwv explain {token} failed");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let printed = common::strip_version_banner(&stdout);
        let printed = printed.trim_end_matches('\n');

        // Compared against the PAGE, not against `entry_for_token` — that is
        // the function under test, and asserting it against itself passes
        // whatever it becomes. What must hold is that the bytes an operator
        // reads occur verbatim in what the site publishes.
        let published = pages_text();
        assert!(
            !printed.is_empty() && published.contains(printed),
            "`rwv explain {token}` printed text that is not in any published page:\n{printed}"
        );
        assert!(
            printed.starts_with(&format!("### `{token}`"))
                || printed.starts_with(&format!("#### `{token}`")),
            "`rwv explain {token}` opened with something other than that token's heading:\n{printed}"
        );
    }
}

/// The typo hint spans tokens, not only verbs — a mistyped token is the input
/// this arm exists for now that tokens outnumber verbs five to one.
#[test]
fn a_mistyped_token_is_suggested() {
    let out = common::rwv()
        .args(["explain", "stale-lok"])
        .output()
        .expect("rwv should run");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("did you mean: stale-lock"),
        "expected a token suggestion, got:\n{stderr}"
    );
}

/// A verb still serves its bundle now that tokens resolve too, and — the half
/// that would otherwise make this vacuous — no token currently collides with a
/// verb name, so the precedence rule is unexercised rather than proven.
///
/// Both assertions are here on purpose. Dropping the precedence guard leaves
/// the first one green, because there is no colliding token for a token-first
/// order to prefer. The second says so out loud, and fires on the day someone
/// mints a token named for a verb — which is when the order stops being
/// theoretical and someone has to choose deliberately.
#[test]
fn a_verb_serves_its_bundle_and_no_token_contests_one() {
    let out = common::rwv()
        .args(["explain", "lock"])
        .output()
        .expect("rwv should run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let printed = common::strip_version_banner(&stdout);
    assert!(
        printed.starts_with("# rwv lock"),
        "the verb bundle must win, got:\n{}",
        &printed[..printed.len().min(120)]
    );

    let verbs: std::collections::BTreeSet<&str> = repoweave::explain::known_verbs().collect();
    let contested: Vec<&str> = repoweave::explain::documented_tokens()
        .into_iter()
        .filter(|t| verbs.contains(t))
        .collect();
    assert!(
        contested.is_empty(),
        "these tokens are spelled like verbs, so `rwv explain` now has a real \
         precedence decision to make rather than a hypothetical one: {contested:?}"
    );
}

/// Every entry the state-to-choices table generates, wherever its page is:
/// the refusals page's, and the findings page's for a kind doctor also reports.
fn generated_entries() -> String {
    let vcs = repoweave::manifest::VcsType::Git;
    let findings: Vec<String> = repoweave::diagnosis::finding_entries_markdown(vcs)
        .into_iter()
        .map(|(_, entry)| entry)
        .collect();
    format!(
        "{}\n{}",
        repoweave::diagnosis::kind_entries_markdown(vcs),
        findings.join("\n")
    )
}

/// Each kind's entry on its page is the one the state-to-choices table
/// generates, byte for byte, and `rwv explain <kind>` reaches it: the page is
/// the table's output, not a hand copy that could say something the refusal
/// does not print.
#[test]
fn every_kind_serves_the_entry_the_table_generates() {
    let generated = generated_entries();
    let kinds = kind_tokens();
    assert!(
        kinds.len() >= 4 && kinds.contains(&"edits".to_owned()),
        "the kind walk read {kinds:?}"
    );
    for kind in &kinds {
        let entry = repoweave::explain::entry_for_token(kind)
            .unwrap_or_else(|| panic!("`rwv explain {kind}` reaches no entry"));
        assert!(
            entry.starts_with(&format!("### `{kind}`\n")),
            "`{kind}` is served from another heading:\n{entry}"
        );
        assert!(
            generated.contains(entry),
            "the page's `{kind}` entry is not what the table generates; regenerate with \
             `cargo run --example generate-explain`:\n{entry}"
        );
    }
}

/// The seeded failure for the pin above: an entry that differs from the
/// table's by one word is reported, for a kind of each page.
#[test]
fn a_kind_entry_edited_by_hand_is_not_the_generated_one() {
    let generated = generated_entries();
    for (kind, phrase, edit) in [
        ("edits", "commit your edits", "commit your work"),
        (
            "unparseable-project",
            "committing only that",
            "committing everything",
        ),
    ] {
        let entry = repoweave::explain::entry_for_token(kind).expect("the kind is served");
        let edited = entry.replacen(phrase, edit, 1);
        assert_ne!(edited, entry, "the seed must change the `{kind}` entry");
        assert!(!generated.contains(&edited));
    }
}

/// A kind the table shares with a doctor finding has its one entry on the
/// findings page and none on the refusals page, and a kind it does not share
/// has its one entry on the refusals page; `rwv explain` resolves each, and
/// the shared one's sub-kinds too. The token a refusal printed as a bare
/// condition, before the kind existed, is no longer a refusal token.
#[test]
fn a_kind_shared_with_a_finding_has_exactly_one_entry_and_explain_resolves_it() {
    let findings = finding_tokens();
    let pages = entry_pages();
    let at = entry_locations(&pages);
    let shared: Vec<String> = kind_tokens()
        .into_iter()
        .filter(|k| findings.contains_key(k))
        .collect();
    assert_eq!(
        shared,
        [
            "alignment",
            "history-disagreement",
            "legacy-workweave-index",
            "missing-lock-merge-driver",
            "pin",
            "unparseable-project"
        ],
        "the kinds doctor also reports"
    );

    for kind in kind_tokens() {
        let want = if findings.contains_key(&kind) {
            vec!["doctor-findings.md"]
        } else {
            vec!["refusals.md"]
        };
        assert_eq!(
            at.get(&kind),
            Some(&want),
            "`{kind}` has entries on the wrong pages"
        );
        assert!(
            repoweave::explain::entry_for_token(&kind).is_some(),
            "`rwv explain {kind}` resolves to nothing"
        );
    }
    for sub_kind in ["pre-schema", "malformed"] {
        let name = format!("unparseable-project/{sub_kind}");
        let entry = repoweave::explain::entry_for_token(&name)
            .unwrap_or_else(|| panic!("`rwv explain {name}` resolves to nothing"));
        assert!(entry.starts_with(&format!("#### `{sub_kind}`")), "{entry}");
    }
    assert!(
        !minted_tokens().contains(&"unparseable-project".to_owned()),
        "a token refusal for a kind the table carries would be a second way to refuse it"
    );
}
