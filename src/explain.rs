//! `rwv explain <cmd>` — per-verb agent-oriented reflection.
//!
//! Returns a markdown bundle for the requested verb (purpose, invocation,
//! output description with JSON Schema for `--json`-capable verbs).
//!
//! Implementation: build-time generator (`cargo run --bin generate-explain`)
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

/// The published pages an entry can live on, in resolution order.
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

/// The heading level of `line` when it is a heading titled exactly `` `token` ``.
fn entry_heading_level(line: &str, token: &str) -> Option<usize> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    (hashes >= 2 && line[hashes..].trim() == format!("`{token}`")).then_some(hashes)
}

/// The entry for `token` on `page`: its heading, and everything under it up to
/// the next heading at the same level or above.
///
/// Sliced rather than copied, so what `rwv explain` prints and what the page
/// publishes cannot drift into two spellings — they are the same bytes.
fn entry_on_page(page: &'static str, token: &str) -> Option<&'static str> {
    let mut start = None;
    let mut level = 0;
    for (offset, line) in line_offsets(page) {
        match start {
            None => {
                if let Some(l) = entry_heading_level(line, token) {
                    start = Some(offset);
                    level = l;
                }
            }
            Some(from) => {
                let hashes = line.len() - line.trim_start_matches('#').len();
                if hashes > 0 && hashes <= level && line[hashes..].starts_with(' ') {
                    return Some(page[from..offset].trim_end_matches('\n'));
                }
            }
        }
    }
    start.map(|from| page[from..].trim_end_matches('\n'))
}

fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n').map(move |raw| {
        let at = offset;
        offset += raw.len();
        (at, raw.trim_end_matches('\n'))
    })
}

/// The entry for `token`, from whichever published page documents it.
pub fn entry_for_token(token: &str) -> Option<&'static str> {
    ENTRY_PAGES
        .iter()
        .find_map(|page| entry_on_page(page, token))
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
    ("lock", LOCK_EXPLAIN),
    ("activate", ACTIVATE_EXPLAIN),
    ("materialize", MATERIALIZE_EXPLAIN),
    ("init", INIT_EXPLAIN),
];

/// Whether `name` is a core verb `rwv explain` serves a bundle for.
fn is_known_verb(name: &str) -> bool {
    VERB_BUNDLES.iter().any(|&(v, _)| v == name)
}

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

/// Return the closest verb from [`known_verbs`] if its edit distance from
/// `input` is within the suggestion threshold, otherwise `None`.
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
/// an unrecognised name is owed.
/// The nearest thing `rwv explain` could have served, over both the verbs and
/// the documented tokens — the two vocabularies a reader types into it.
fn suggest(input: &str) -> Option<&'static str> {
    const THRESHOLD: usize = 2;
    known_verbs()
        .chain(documented_tokens())
        .map(|v| (v, levenshtein(input, v)))
        .filter(|&(v, d)| d <= THRESHOLD && d < v.chars().count())
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
        // A verb first, then a token. Verbs win a collision because that is
        // the name the reader typed a command with a moment ago.
        Some(verb) if entry_for_token(verb).is_some() && !is_known_verb(verb) => {
            println!("{}", entry_for_token(verb).expect("just matched"));
        }
        Some(verb) => match VERB_BUNDLES.iter().find(|&&(name, _)| name == verb) {
            Some(&(_, bundle)) => print!("{bundle}"),
            // Non-core verb: explain is reflection over core's committed,
            // CI-checked surfaces. Extending it to exec third-party binaries
            // would make rwv's reflection surface only as trustworthy as the
            // least trustworthy thing on `$PATH`, so explain never touches
            // PATH content. A close-typo hint still fires when the input is
            // within edit distance of a core verb — that's an operator help,
            // not a plugin dispatch. Any other name is redirected to the
            // plugin's own `--help`, which is the plugin's responsibility to
            // document.
            None => {
                if let Some(candidate) = suggest(verb) {
                    crate::refuse!(
                        crate::refusal::RefusalKind::NoExplainEntry,
                        "no explain entry for '{verb}'; did you mean: {candidate}? \
                         Try `rwv explain` for the full index."
                    );
                } else {
                    crate::refuse!(
                        crate::refusal::RefusalKind::NoExplainEntry,
                        "external command; try `rwv {verb} --help`"
                    );
                }
            }
        },
    }
    Ok(())
}
