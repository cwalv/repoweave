//! `rwv explain` reaches every entry on the published pages, a sub-kind's
//! included, through a spelling that names it and nothing else.
//!
//! A sub-kind is written under its kind, and two kinds may carry the same
//! sub-kind with different repairs: `safe-to-fix` is one of `index-drift` and
//! one of `working-tree-drift`. A bare name serving either entry leaves the
//! other unreachable, so a finding's `<kind>/<sub-kind>` names its entry, and
//! a bare name heading entries under two kinds refuses rather than pick one.

use std::path::Path;

mod common;

/// One entry heading as the page itself lays it out, read here rather than
/// through the module that serves it.
#[derive(Debug, Clone)]
struct PageEntry {
    page: &'static str,
    token: String,
    kind: Option<String>,
    text: String,
}

impl PageEntry {
    fn spellings(&self) -> Vec<String> {
        let mut out = vec![self.token.clone()];
        if let Some(kind) = &self.kind {
            out.push(format!("{kind}/{}", self.token));
        }
        out
    }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.len() - line.trim_start_matches('#').len();
    (level > 0 && line[level..].starts_with(' ')).then(|| (level, line[level..].trim()))
}

/// Every entry on the three entry pages: a heading at level two or deeper
/// titled by one backticked token, the text up to the next heading at its
/// level or above, and the token the nearest shallower heading opens with.
fn page_entries() -> Vec<PageEntry> {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference");
    let mut out = Vec::new();
    for page in ["refusals.md", "doctor-findings.md", "vcs-errors.md"] {
        let text = std::fs::read_to_string(docs.join(page)).expect("the page is published");
        let mut offset = 0;
        let mut headings = Vec::new();
        for raw in text.split_inclusive('\n') {
            if let Some((level, title)) = heading(raw.trim_end_matches('\n')) {
                headings.push((offset, level, title.to_owned()));
            }
            offset += raw.len();
        }
        for (i, (start, level, title)) in headings.iter().enumerate() {
            let Some(token) = title
                .strip_prefix('`')
                .and_then(|t| t.strip_suffix('`'))
                .filter(|t| *level >= 2 && !t.is_empty() && !t.contains([' ', '`']))
            else {
                continue;
            };
            let end = headings[i + 1..]
                .iter()
                .find(|(_, l, _)| l <= level)
                .map_or(text.len(), |(o, _, _)| *o);
            let kind = headings[..i]
                .iter()
                .rev()
                .find(|(_, l, _)| l < level)
                .and_then(|(_, _, t)| t.strip_prefix('`')?.split('`').next())
                .map(str::to_owned);
            out.push(PageEntry {
                page,
                token: token.to_owned(),
                kind,
                text: text[*start..end].trim_end_matches('\n').to_owned(),
            });
        }
    }
    out
}

/// The entries no spelling reaches under `resolve`.
fn unreachable(entries: &[PageEntry], resolve: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| {
            !entry
                .spellings()
                .iter()
                .any(|s| resolve(s).as_deref() == Some(entry.text.as_str()))
        })
        .map(|entry| match &entry.kind {
            Some(kind) => format!("{}: `{}` under `{kind}`", entry.page, entry.token),
            None => format!("{}: `{}`", entry.page, entry.token),
        })
        .collect()
}

fn served(name: &str) -> Option<String> {
    repoweave::explain::entry_for_token(name).map(str::to_owned)
}

#[test]
fn every_entry_is_reachable_through_some_spelling() {
    let entries = page_entries();
    let sub_kind_entries = entries
        .iter()
        .filter(|e| e.page == "doctor-findings.md" && e.kind.is_some())
        .count();
    assert!(
        entries.len() > 150 && sub_kind_entries > 50,
        "the page walk read {} entries, {sub_kind_entries} of them sub-kinds: it has \
         stopped reading the pages",
        entries.len()
    );

    let missing = unreachable(&entries, &served);
    assert!(
        missing.is_empty(),
        "no `rwv explain` spelling serves these entries:\n{missing:#?}"
    );

    let qualified_only: Vec<String> = entries
        .iter()
        .filter(|e| served(&e.token).as_deref() != Some(e.text.as_str()))
        .map(|e| format!("{}/{}", e.kind.as_deref().unwrap_or("?"), e.token))
        .collect();
    assert_eq!(
        qualified_only,
        ["index-drift/safe-to-fix", "working-tree-drift/safe-to-fix"],
        "the entries only a qualified spelling reaches"
    );
}

/// The seeded failure: a resolver that serves a bare name's first entry and
/// knows no qualified spelling leaves the second `safe-to-fix` unreachable.
#[test]
fn a_shadowed_sub_kind_entry_is_reported() {
    let entries = page_entries();
    let first_entry = |name: &str| {
        entries
            .iter()
            .find(|e| e.token == name)
            .map(|e| e.text.clone())
    };
    assert_eq!(
        unreachable(&entries, &first_entry),
        ["doctor-findings.md: `safe-to-fix` under `working-tree-drift`"]
    );
}

fn page_entry(kind: &str, token: &str) -> PageEntry {
    page_entries()
        .into_iter()
        .find(|e| e.token == token && e.kind.as_deref() == Some(kind))
        .unwrap_or_else(|| panic!("the findings page has `{token}` under `{kind}`"))
}

fn explain(name: &str) -> std::process::Output {
    common::rwv()
        .args(["explain", name])
        .output()
        .expect("rwv should run")
}

fn served_by_binary(name: &str) -> String {
    let out = explain(name);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "`rwv explain {name}` failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    common::strip_version_banner(&stdout)
        .trim_end_matches('\n')
        .to_owned()
}

fn refused(name: &str) -> String {
    let out = explain(name);
    assert!(
        !out.status.success(),
        "`rwv explain {name}` was expected to refuse"
    );
    assert!(out.stdout.is_empty(), "a refusal prints no page");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_kind_and_sub_kind_reach_that_kinds_entry() {
    let index = served_by_binary("index-drift/safe-to-fix");
    let working_tree = served_by_binary("working-tree-drift/safe-to-fix");
    assert_eq!(index, page_entry("index-drift", "safe-to-fix").text);
    assert_eq!(
        working_tree,
        page_entry("working-tree-drift", "safe-to-fix").text
    );
    assert_ne!(index, working_tree);
}

#[test]
fn a_sub_kind_two_kinds_carry_refuses_naming_both() {
    let stderr = refused("safe-to-fix");
    for spelling in ["index-drift/safe-to-fix", "working-tree-drift/safe-to-fix"] {
        assert!(
            stderr.contains(&format!("`rwv explain {spelling}`")),
            "the refusal must name `{spelling}`:\n{stderr}"
        );
    }
    assert!(
        stderr.ends_with("\n\nrwv explain ambiguous-explain-token\n"),
        "{stderr}"
    );
}

#[test]
fn a_sub_kind_one_kind_carries_resolves_bare_and_qualified_alike() {
    let bare = served_by_binary("live-staged");
    assert_eq!(bare, page_entry("index-drift", "live-staged").text);
    assert_eq!(served_by_binary("index-drift/live-staged"), bare);
}

/// `member-permissions` has one entry for both its sub-kinds, so the kind's
/// entry is the one a finding of either reaches.
#[test]
fn a_sub_kind_with_no_entry_of_its_own_reaches_its_kinds_entry() {
    let kind = served_by_binary("member-permissions");
    assert!(kind.starts_with("### `member-permissions`"), "{kind}");
    assert_eq!(
        served_by_binary("member-permissions/writable-reference"),
        kind
    );
    assert_eq!(
        served_by_binary("member-permissions/read-only-member"),
        kind
    );
}

#[test]
fn a_sub_kind_its_kind_does_not_carry_reaches_nothing() {
    for name in [
        "index-drift/live-edits",
        "stale-lock/safe-to-fix",
        "member-permissions/safe-to-fix",
        "index-drift/",
    ] {
        let stderr = refused(name);
        assert!(
            stderr.ends_with("\n\nrwv explain no-explain-entry\n")
                && !stderr.contains("external command"),
            "`rwv explain {name}`:\n{stderr}"
        );
    }
}

#[test]
fn a_mistyped_qualified_name_is_suggested() {
    let stderr = refused("index-drift/safe-to-fx");
    assert!(
        stderr.contains("did you mean: index-drift/safe-to-fix?"),
        "{stderr}"
    );
}

/// Every `<kind>/<sub-kind>` a finding can carry, read off the doctor schema.
fn qualified_spellings() -> Vec<(String, String)> {
    repoweave::check::finding_sub_kinds()
        .into_iter()
        .flat_map(|(kind, subs)| subs.into_iter().map(move |sub| (kind.clone(), sub)))
        .collect()
}

/// The token an entry's heading names.
fn heading_token(entry: &str) -> &str {
    entry
        .lines()
        .next()
        .and_then(|line| line.trim_start_matches('#').trim().strip_prefix('`'))
        .and_then(|rest| rest.strip_suffix('`'))
        .unwrap_or_else(|| panic!("an entry opens with its token's heading:\n{entry}"))
}

#[test]
fn every_finding_sub_kind_resolves_by_its_qualified_spelling() {
    let spellings = qualified_spellings();
    assert!(
        spellings.len() > 50 && spellings.contains(&("index-drift".into(), "live-staged".into())),
        "the register walk read {} sub-kinds: it has stopped reading the schema",
        spellings.len()
    );
    let unserved: Vec<String> = spellings
        .iter()
        .filter_map(|(kind, sub)| {
            let name = format!("{kind}/{sub}");
            match repoweave::explain::entry_for_token(&name) {
                Some(entry) if [kind.as_str(), sub.as_str()].contains(&heading_token(entry)) => {
                    None
                }
                Some(entry) => Some(format!("`{name}` serves `{}`", heading_token(entry))),
                None => Some(format!("`{name}` serves nothing")),
            }
        })
        .collect();
    assert!(unserved.is_empty(), "{unserved:#?}");
}

/// The kinds whose sub-kinds have entries of their own for some but not all
/// of them: the kind's entry would then answer for a sub-kind it says nothing
/// about.
fn partially_covered_kinds(entries: &[PageEntry], spellings: &[(String, String)]) -> Vec<String> {
    let mut kinds: Vec<&str> = spellings.iter().map(|(kind, _)| kind.as_str()).collect();
    kinds.dedup();
    kinds
        .into_iter()
        .filter(|kind| {
            let covered: Vec<bool> = spellings
                .iter()
                .filter(|(k, _)| k == kind)
                .map(|(_, sub)| {
                    entries.iter().any(|e| {
                        e.page == "doctor-findings.md"
                            && e.kind.as_deref() == Some(*kind)
                            && &e.token == sub
                    })
                })
                .collect();
            covered.contains(&true) && covered.contains(&false)
        })
        .map(str::to_owned)
        .collect()
}

/// Structural, over an enumerable population: every kind the doctor schema
/// gives a `sub_kind`, against the entry headings of the findings page. What
/// it does not read is whether the kind's entry, where it covers its
/// sub-kinds, says anything about each of them.
#[test]
fn a_kind_gives_every_sub_kind_an_entry_or_none() {
    let entries = page_entries();
    let spellings = qualified_spellings();
    assert_eq!(
        partially_covered_kinds(&entries, &spellings),
        Vec::<String>::new()
    );
}

#[test]
fn a_kind_missing_one_sub_kind_entry_is_reported() {
    let entries: Vec<PageEntry> = page_entries()
        .into_iter()
        .filter(|e| !(e.token == "live-staged" && e.kind.as_deref() == Some("index-drift")))
        .collect();
    assert_eq!(
        partially_covered_kinds(&entries, &qualified_spellings()),
        ["index-drift"]
    );
}
