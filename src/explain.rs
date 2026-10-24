//! `rwv explain <cmd>` — per-verb agent-oriented reflection.
//!
//! Returns a markdown bundle for the requested verb (purpose, invocation,
//! output description with JSON Schema for `--json`-capable verbs).
//!
//! Implementation: build-time generator (`cargo run --example generate-explain`)
//! assembles markdown from hand-written templates + schemars-derived JSON
//! schemas, writing artifacts to `docs/reference/explain/*.md` and
//! `docs/reference/schemas/*.json`. This module embeds those artifacts via
//! `include_str!()` and dispatches with a trivial lookup.

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
const ROLE_EXPLAIN: &str = include_str!("../docs/reference/explain/role.md");
const LOCK_EXPLAIN: &str = include_str!("../docs/reference/explain/lock.md");
const ACTIVATE_EXPLAIN: &str = include_str!("../docs/reference/explain/activate.md");
const MATERIALIZE_EXPLAIN: &str = include_str!("../docs/reference/explain/materialize.md");
const INIT_EXPLAIN: &str = include_str!("../docs/reference/explain/init.md");

// Hand-written entry pages. Unlike the bundles above these are not generated:
// an entry says why a rule exists and which exit applies when, which is not
// derivable from the code it describes.
const REFUSALS_PAGE: &str = include_str!("../docs/reference/refusals.md");
const DOCTOR_FINDINGS_PAGE: &str = include_str!("../docs/reference/doctor-findings.md");
const VCS_ERRORS_PAGE: &str = include_str!("../docs/reference/vcs-errors.md");

/// The published pages an entry can live on.
///
/// A token names one condition, so it has one entry wherever that entry
/// already lives — a refusal reporting a state `rwv doctor` also reports is
/// served from the findings page rather than given a second entry here. That
/// is why this is a list of pages and not a page: nothing about serving an
/// entry may depend on which page carries it.
///
/// The vocabulary this resolves over is every register that publishes an
/// operator-facing token, **the integration channel's issue kinds included**.
/// That inclusion is the one a reader is likely to get wrong: those kinds
/// arrive on a separate `--json` array, from hooks rather than from rwv's own
/// scans, and neither fact takes them out of the index. An issue kind is
/// looked up the same way, from these same pages.
///
/// One consequence, visible on the findings page and easy to read as an
/// oversight: entries for issue kinds open with prose where an entry for a
/// `violations` finding opens with an Auto-fixable / Report-only mark. A mark
/// states a disposition for a whole kind, and on that channel what `--fix` may
/// touch is settled per finding and carried on it — so a mark keyed to the
/// kind would be false for some of the findings that kind covers.
///
/// The VCS wire kinds and sync's per-repo failure kinds read least like the
/// others: rwv did not decline, it was stopped, so they sit outside the
/// refusal class and share a page of their own rather than borrowing a refusal
/// token. Nothing about serving an entry depends on which page holds it, which
/// is what makes another page a listing rather than a special case.
const ENTRY_PAGES: &[&str] = &[REFUSALS_PAGE, DOCTOR_FINDINGS_PAGE, VCS_ERRORS_PAGE];

fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    (hashes > 0 && line[hashes..].starts_with(' ')).then_some(hashes)
}

/// The token a heading's title opens with, when it opens with one in backticks.
fn opening_token(title: &str) -> Option<&str> {
    title.trim().strip_prefix('`')?.split('`').next()
}

/// One entry: the slice of its page from its heading to the next heading at
/// the same level or above, and the finding kind whose section it sits in.
///
/// Sliced rather than copied, so what `rwv explain` prints and what the page
/// publishes cannot drift into two spellings — they are the same bytes.
struct Entry {
    kind: Option<&'static str>,
    text: &'static str,
}

/// Every entry headed exactly `` `token` ``, on every page.
///
/// A sub-kind entry's kind is the token the nearest heading above it at a
/// shallower level opens with; that is how a page nests a sub-kind under its
/// kind, including under a heading that goes on to qualify the kind in words.
fn entries_headed(token: &str) -> Vec<Entry> {
    let mut found = Vec::new();
    for page in ENTRY_PAGES {
        let headings: Vec<(usize, usize, &'static str)> = line_offsets(page)
            .filter_map(|(offset, line)| {
                let level = heading_level(line)?;
                Some((offset, level, &line[level..]))
            })
            .collect();
        for (i, &(start, level, title)) in headings.iter().enumerate() {
            if level < 2 || title.trim() != format!("`{token}`") {
                continue;
            }
            let end = headings[i + 1..]
                .iter()
                .find(|&&(_, l, _)| l <= level)
                .map_or(page.len(), |&(offset, _, _)| offset);
            let kind = headings[..i]
                .iter()
                .rev()
                .find(|&&(_, l, _)| l < level)
                .and_then(|&(_, _, title)| opening_token(title));
            found.push(Entry {
                kind,
                text: page[start..end].trim_end_matches('\n'),
            });
        }
    }
    found
}

fn line_offsets(text: &'static str) -> impl Iterator<Item = (usize, &'static str)> {
    let mut offset = 0;
    text.split_inclusive('\n').map(move |raw| {
        let at = offset;
        offset += raw.len();
        (at, raw.trim_end_matches('\n'))
    })
}

/// What `rwv explain` finds for a name that is not a verb.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// The one entry the name reaches.
    Entry(&'static str),
    /// A sub-kind that more than one finding kind has an entry for, with every
    /// `<kind>/<sub-kind>` spelling that reaches one of those entries.
    Ambiguous(Vec<String>),
    /// No entry on any page.
    Unknown,
}

/// The entry for `name`: a token as an entry heading spells it, or a finding's
/// `<kind>/<sub-kind>`.
///
/// A qualified name reaches the sub-kind's own entry in its kind's section,
/// or the kind's entry for a kind whose one entry covers all its sub-kinds.
/// A page gives either every sub-kind of a kind an entry or none of them, so
/// the kind's entry is never served for a sub-kind it does not cover. A bare
/// name heading entries under two kinds reaches neither: which one the reader
/// meant is not written in it.
pub fn lookup(name: &str) -> Lookup {
    if let Some((kind, sub_kind)) = name.split_once('/') {
        let carried = crate::check::finding_sub_kinds()
            .get(kind)
            .is_some_and(|sub_kinds| sub_kinds.contains(sub_kind));
        if !carried {
            return Lookup::Unknown;
        }
        return match entries_headed(sub_kind)
            .into_iter()
            .find(|entry| entry.kind == Some(kind))
        {
            Some(entry) => Lookup::Entry(entry.text),
            None => match lookup(kind) {
                Lookup::Entry(text) => Lookup::Entry(text),
                Lookup::Ambiguous(_) | Lookup::Unknown => Lookup::Unknown,
            },
        };
    }
    let mut entries = entries_headed(name);
    match entries.len() {
        0 => Lookup::Unknown,
        1 => Lookup::Entry(entries.remove(0).text),
        _ => Lookup::Ambiguous(
            entries
                .iter()
                .filter_map(|entry| Some(format!("{}/{name}", entry.kind?)))
                .collect(),
        ),
    }
}

/// The entry `rwv explain` serves for `token`, if it serves exactly one.
pub fn entry_for_token(token: &str) -> Option<&'static str> {
    match lookup(token) {
        Lookup::Entry(text) => Some(text),
        Lookup::Ambiguous(_) | Lookup::Unknown => None,
    }
}

/// Every token the published entry pages document, derived by reading them.
///
/// The register is not restated here: a hand-written copy of it is a second
/// list to keep in step with the enum, and this one is read from the pages an
/// operator is actually served.
pub fn documented_tokens() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = ENTRY_PAGES
        .iter()
        .flat_map(|page| {
            line_offsets(page).filter_map(|(_, line)| {
                let hashes = line.len() - line.trim_start_matches('#').len();
                let rest = line.get(hashes..)?.trim();
                (hashes >= 2 && rest.starts_with('`') && rest.ends_with('`') && rest.len() > 2)
                    .then(|| rest.trim_matches('`'))
            })
        })
        .filter(|t| !t.contains(' '))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Every verb `rwv explain` recognizes, paired with its embedded bundle, in
/// index order. The one registry: dispatch and [`known_verbs`] both read it,
/// so a verb added or removed here changes both at once.
const VERB_BUNDLES: &[(&str, &str)] = &[
    ("status", STATUS_EXPLAIN),
    ("doctor", DOCTOR_EXPLAIN),
    ("sync", SYNC_EXPLAIN),
    ("sync-to", SYNC_TO_EXPLAIN),
    ("fetch", FETCH_EXPLAIN),
    ("update", UPDATE_EXPLAIN),
    ("push", PUSH_EXPLAIN),
    ("prime", PRIME_EXPLAIN),
    ("explain", EXPLAIN_EXPLAIN),
    ("workweave", WORKWEAVE_EXPLAIN),
    ("abort", ABORT_EXPLAIN),
    ("add", ADD_EXPLAIN),
    ("remove", REMOVE_EXPLAIN),
    ("role", ROLE_EXPLAIN),
    ("lock", LOCK_EXPLAIN),
    ("activate", ACTIVATE_EXPLAIN),
    ("materialize", MATERIALIZE_EXPLAIN),
    ("init", INIT_EXPLAIN),
];

/// The complete set of verbs recognized by `rwv explain`, in index order.
pub fn known_verbs() -> impl Iterator<Item = &'static str> {
    VERB_BUNDLES.iter().map(|&(name, _)| name)
}

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

/// The nearest thing `rwv explain` could have served — a verb, a documented
/// token, or a finding's `<kind>/<sub-kind>` — when its edit distance from
/// `input` is within the suggestion threshold.
///
/// Threshold: distance ≤ 2. This accepts single-character typos up to
/// two-character transpositions while excluding completely unrelated words
/// like "frobnicate".
///
/// The distance must also be shorter than the candidate itself, or the
/// "typo" rewrites the whole candidate rather than corrupting part of it. A
/// flat threshold is a statement about typos only while every candidate is
/// longer than the threshold; a two-character token like `io` is within
/// distance 2 of every string of length four or less, so without this guard
/// it answers for inputs it has nothing to do with — and it answers *first*,
/// because a spurious match here suppresses the external-command pointer that
/// an unrecognized name is owed.
fn suggest(input: &str) -> Option<String> {
    const THRESHOLD: usize = 2;
    let qualified = crate::check::finding_sub_kinds()
        .into_iter()
        .flat_map(|(kind, subs)| subs.into_iter().map(move |sub| format!("{kind}/{sub}")));
    known_verbs()
        .chain(documented_tokens())
        .map(str::to_owned)
        .chain(qualified)
        .map(|candidate| {
            let distance = levenshtein(input, &candidate);
            (candidate, distance)
        })
        .filter(|(candidate, d)| *d <= THRESHOLD && *d < candidate.chars().count())
        .min_by_key(|(_, d)| *d)
        .map(|(candidate, _)| candidate)
}

/// The line every served `explain` page opens with: the same string `rwv
/// --version` reports, so a committed page can go stale with the release that
/// cut it without lying about which binary printed it.
fn version_banner() -> String {
    format!("rwv {}\n\n", crate::rwv_version_with_build())
}

/// Write a page to stdout.
fn emit(page: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    crate::stdout::write_bytes(page.as_bytes())
        .and_then(|()| crate::stdout::flush())
        .context("failed to write the explain page to stdout")
}

/// Print the explain bundle for the requested verb, the entry for a token, or
/// the index when none given.
///
/// Returns an error only when the name reaches no page, or reaches more than
/// one entry.
pub fn explain(cmd: Option<&str>) -> anyhow::Result<()> {
    let Some(name) = cmd else {
        return emit(&format!("{}{INDEX_EXPLAIN}", version_banner()));
    };
    // A verb first, then a token. Verbs win a collision because that is
    // the name the reader typed a command with a moment ago.
    if let Some(&(_, bundle)) = VERB_BUNDLES.iter().find(|&&(verb, _)| verb == name) {
        return emit(&format!("{}{bundle}", version_banner()));
    }
    match lookup(name) {
        Lookup::Entry(entry) => emit(&format!("{}{entry}\n", version_banner())),
        Lookup::Ambiguous(spellings) => crate::refuse!(
            crate::refusal::RefusalKind::AmbiguousExplainToken,
            "`{name}` is a sub-kind of more than one finding kind, each with its own \
             entry; name the one you mean: {}",
            spellings
                .iter()
                .map(|spelling| format!("`rwv explain {spelling}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        // Non-core verb: explain is reflection over core's committed,
        // CI-checked surfaces. Extending it to exec third-party binaries
        // would make rwv's reflection surface only as trustworthy as the
        // least trustworthy thing on `$PATH`, so explain never touches
        // PATH content. A close-typo hint still fires when the input is
        // within edit distance of a core verb — that's an operator help,
        // not a plugin dispatch. Any other name is redirected to the
        // plugin's own `--help`, which is the plugin's responsibility to
        // document; a name with a `/` in it is no plugin's.
        Lookup::Unknown => match suggest(name) {
            Some(candidate) => crate::refuse!(
                crate::refusal::RefusalKind::NoExplainEntry,
                "no explain entry for '{name}'; did you mean: {candidate}? \
                 Try `rwv explain` for the full index."
            ),
            None if name.contains('/') => crate::refuse!(
                crate::refusal::RefusalKind::NoExplainEntry,
                "no explain entry for '{name}'. Try `rwv explain` for the full index."
            ),
            None => crate::refuse!(
                crate::refusal::RefusalKind::NoExplainEntry,
                "external command; try `rwv {name} --help`"
            ),
        },
    }
}
