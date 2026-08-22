//! The VCS wire register's entries, in both directions.
//!
//! `token_register_agreement_test.rs` already asserts that every published
//! token has an entry heading on one of the published pages. It reads those
//! pages off disk, which is the right thing for a claim about the pages — and
//! the wrong thing for a claim about `rwv explain`, which serves from
//! `explain::ENTRY_PAGES`. The two can disagree: a page added to the walk and
//! not to `ENTRY_PAGES` leaves the walk green while the command an operator is
//! told to run says the token is unknown. That gap is what the forward test
//! here closes, and it closes it through the binary for the same reason
//! `refusal_entry_test.rs` does — the claim is about what an operator reads.
//!
//! The reverse direction has no other home. `every_entry_names_a_minted_token`
//! guards refusals.md against entries that outlived their tokens; nothing
//! guards this page but the test below.
//!
//! The page carries two registers, and the reverse direction has to read both
//! or it reports one register's entries as the other's orphans. Sync's failure
//! kinds are documented here because they arrive in one JSON object with the
//! VCS kinds — sync's as the outer tag, the VCS one as the `cause` beneath it
//! — so a reader holding a failed repo outcome looks both up in one place.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn page() -> String {
    std::fs::read_to_string(manifest_dir().join("docs/reference/vcs-errors.md"))
        .expect("the vcs errors page is published")
}

/// The `### `token`` headings on the page.
fn entries() -> BTreeSet<String> {
    page()
        .lines()
        .filter_map(|l| l.strip_prefix("### `")?.strip_suffix('`'))
        .map(str::to_owned)
        .collect()
}

/// Tokens the VCS wire register publishes, read out of the one function that
/// mints them rather than listed here.
///
/// **One source, and which one is the whole point.** `VcsError::kind` is the
/// register; `ConflictOp`'s `Display` is not. Those spellings — `rebase`,
/// `merge`, `cherry-pick` — are values of a *field* on `rebase-conflict`, not
/// discriminants a consumer branches on, and they are documented inside that
/// entry rather than as entries of their own. A walk that swept the whole file
/// for `=> "literal"` arms collected `cherry-pick` and not its two siblings,
/// because that walk also required a hyphen and only one of the three has one.
/// Reading the minting function is what makes the population the register
/// rather than an artifact of the spelling.
fn minted(file: &str) -> BTreeSet<String> {
    let src = std::fs::read_to_string(manifest_dir().join("src").join(file))
        .unwrap_or_else(|e| panic!("src/{file} reads: {e}"));
    const MINT: &str = "pub fn kind(&self) -> &'static str {";
    let body = src
        .split_once(MINT)
        .unwrap_or_else(|| panic!("src/{file} still declares {MINT:?}"))
        .1
        .split_once("\n    }")
        .expect("the minting function still closes")
        .0;
    let mut out = BTreeSet::new();
    let mut rest = body;
    while let Some(at) = rest.find("=> \"") {
        let after = &rest[at + 4..];
        let Some(end) = after.find('"') else { break };
        out.insert(after[..end].to_owned());
        rest = &after[end..];
    }
    out
}

fn published() -> BTreeSet<String> {
    minted("vcs.rs")
}

/// The tokens `SyncFailure::kind` mints — the page's second register.
///
/// Not the wire producer: `rwv sync --json` serialises `SyncFailureOutput`.
/// `sync_failure_kind_matches_wire_tag` pins the two over every variant, which
/// is what makes reading the match arms a faithful read of the wire.
fn sync_failure_kinds() -> BTreeSet<String> {
    minted("sync.rs")
}

/// Published kinds with no entry, and why each is recorded rather than written.
///
/// **Empty.** Every kind `VcsError::kind` mints resolves to an entry — seven on
/// this page, and `mid-operation` and `untracked-collision` from the refusal
/// register, which named those two conditions first and keeps them.
///
/// Asserted exactly in both directions, so a kind arriving without an entry
/// reds, and so does a recorded exemption that has quietly gained one.
const NO_ENTRY: &[(&str, &str)] = &[];

#[test]
fn the_vcs_register_walk_is_not_vacuous() {
    let published = published();
    // Lowered from 11 when three variants nothing constructed were retired off
    // the wire. The floor tracks the register's known size, not a claim that it
    // may only grow.
    assert!(
        published.len() >= 8,
        "the VCS register walk yielded {} tokens; it has stopped matching the \
         arms it reads:\n{published:#?}",
        published.len()
    );
    // `io` is the sentinel that matters: it carries no hyphen, and a walk that
    // required one dropped it silently while looking correct on every other
    // token.
    for expected in ["not-a-repo", "command-failed", "io"] {
        assert!(
            published.contains(expected),
            "the walk lost {expected:?}, which VcsError::kind still mints:\n{published:#?}"
        );
    }
    // The negative half. `cherry-pick` is a ConflictOp field value, not a kind;
    // a walk that reads the whole file instead of the minting function collects
    // it and gives this page an entry nothing routes to.
    assert!(
        !published.contains("cherry-pick"),
        "the walk strayed outside VcsError::kind and picked up a field \
         value:\n{published:#?}"
    );
}

/// A kind's entry lives wherever that entry already lives: `mid-operation` and
/// `untracked-collision` are shared with the refusal register and are served
/// from refusals.md, so the question is whether the token resolves at all, not
/// whether this page carries it.
#[test]
fn every_published_vcs_kind_has_an_entry_or_a_recorded_reason() {
    let recorded: BTreeSet<&str> = NO_ENTRY.iter().map(|(t, _)| *t).collect();
    let resolves = |t: &str| repoweave::explain::entry_for_token(t).is_some();

    let undocumented: Vec<String> = published()
        .into_iter()
        .filter(|t| !resolves(t) && !recorded.contains(t.as_str()))
        .collect();
    assert!(
        undocumented.is_empty(),
        "these VCS wire kinds reach an operator with no entry:\n{undocumented:#?}"
    );

    let stale: Vec<&str> = recorded.iter().copied().filter(|t| resolves(t)).collect();
    assert!(
        stale.is_empty(),
        "these are recorded as having no entry but now resolve — drop them \
         from NO_ENTRY:\n{stale:#?}"
    );
}

/// The forward direction that matters: `rwv explain <kind>` serves the entry.
///
/// Driven through the binary. A library call would pass on the strength of
/// `entry_for_token`, which is the function under test's own machinery; only
/// the process proves dispatch reaches a page that was added to `ENTRY_PAGES`.
#[test]
fn explain_serves_every_entry_on_the_page() {
    let entries = entries();
    assert!(
        entries.len() >= 6,
        "the page walk yielded {} entries; it has stopped reading the page",
        entries.len()
    );

    for token in &entries {
        let out = assert_cmd::Command::cargo_bin("rwv")
            .expect("rwv binary built")
            .args(["explain", token])
            .output()
            .expect("rwv runs");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "`rwv explain {token}` failed; the page carries an entry the \
             command cannot reach:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains(&format!("### `{token}`")),
            "`rwv explain {token}` served something else:\n{stdout}"
        );
    }
}

/// Sync's failure kinds resolve too, and `head-unreadable` is the one that
/// proves the page is not the unit: `rwv doctor` names that condition as well,
/// so its single entry stays on the findings page and this still passes.
#[test]
fn every_sync_failure_kind_has_an_entry() {
    let kinds = sync_failure_kinds();
    assert!(
        kinds.len() >= 3,
        "the sync register walk yielded {} tokens; it has stopped matching the \
         arms it reads:\n{kinds:#?}",
        kinds.len()
    );
    let undocumented: Vec<String> = kinds
        .into_iter()
        .filter(|t| repoweave::explain::entry_for_token(t).is_none())
        .collect();
    assert!(
        undocumented.is_empty(),
        "these sync failure kinds reach an operator with no entry:\n{undocumented:#?}"
    );
}

/// The reverse: an entry naming a token neither register publishes is a section
/// nothing routes to, and it outlives the code silently.
#[test]
fn every_entry_names_a_published_kind() {
    let mut published = published();
    published.extend(sync_failure_kinds());
    let orphaned: Vec<String> = entries()
        .into_iter()
        .filter(|t| !published.contains(t))
        .collect();
    assert!(
        orphaned.is_empty(),
        "these entries outlived the kinds they document:\n{orphaned:#?}"
    );
}
