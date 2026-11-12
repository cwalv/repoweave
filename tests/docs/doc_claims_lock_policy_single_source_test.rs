//! Doc-claim test: the ecosystem-lock commit-vs-gitignore policy is stated
//! once, in `docs/reference/integrations/index.md`, and the per-extension
//! integration pages defer to it rather than asserting their own verdict.
//!
//! Doc claims pinned here:
//!
//!   - `index.md`'s "Committed files and committability" section names
//!     ecosystem lock files as fully rwv-owned and states that rwv mandates
//!     neither commit nor gitignore for them — that choice belongs to the
//!     operator. This is a deliberate silence, not an oversight: rwv can
//!     regenerate the file, so leaving it uncommitted is safe, but nothing
//!     stops an operator from committing it for a checkout that's
//!     reproducible straight from the project repo.
//!   - `docs/reference/integrations/cargo-workspace.md`,
//!     `docs/reference/integrations/npm-workspaces.md` and
//!     `docs/reference/integrations/uv-workspace.md` each
//!     link back to that section wherever they mention their ecosystem's
//!     lock file, instead of asserting the lock's commit status in their
//!     own words.
//!
//! # Why this needs a test at all
//!
//! A deliberate silence reads, to the next person who lands on one of these
//! pages, like an unanswered question — and an unanswered question invites
//! an answer. The failure mode isn't someone arguing rwv *should* mandate a
//! policy; it's someone quietly "fixing" the asymmetry between an
//! unqualified "Committable." and index.md's two-sided framing by picking a
//! side in prose, without touching index.md at all. Nothing else in the
//! build would catch that.
//!
//! # What this does not catch (residue)
//!
//! The per-extension checks below key on two things: the paragraph
//! mentioning the lock file must link back to index.md's section, and it
//! must not contain a small set of known "flat verdict" phrases. A rewrite
//! that reintroduces a commit-status verdict in genuinely novel wording,
//! while still leaving the link in the same paragraph, would not be caught.
//! Catching arbitrary rephrasing would mean parsing English, which is out of
//! reach for a text-matching test; the link requirement is the part of the
//! property that *is* mechanically checkable, and it is the part a
//! "quietly pick a side" edit is most likely to disturb, since restating a
//! verdict in your own words and then also pointing at the page that
//! disagrees with you is an unusual thing to write.

use std::path::Path;

/// Read a doc file relative to the crate root (mirrors the `read_doc`
/// helper in `doc_claims_cli_md_test.rs`).
fn read_doc(rel: &str) -> String {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set (run via cargo test)");
    let path = Path::new(&manifest).join(rel);
    // Paragraph slicing below keys on blank lines; read modulo the eol
    // filter so a CRLF-smudged checkout still has "\n\n" between paragraphs.
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// The token every deferring page must link to.
const INDEX_ANCHOR: &str = "index.md#committed-files-and-committability";

/// Slice out the paragraph (blank-line-delimited) containing `needle`'s
/// first occurrence in `doc`.
fn paragraph_containing<'a>(doc: &'a str, needle: &str) -> &'a str {
    let at = doc
        .find(needle)
        .unwrap_or_else(|| panic!("doc should still mention {needle:?}, got:\n{doc}"));
    let start = doc[..at].rfind("\n\n").map(|i| i + 2).unwrap_or(0);
    let end = doc[at..].find("\n\n").map(|i| at + i).unwrap_or(doc.len());
    &doc[start..end]
}

// ---------------------------------------------------------------------------
// index.md — the single source of the rule
// ---------------------------------------------------------------------------

#[test]
fn index_md_states_lock_commit_policy_as_operator_choice() {
    let index = read_doc("docs/reference/integrations/index.md");
    let section_start = index.find("## Committed files and committability").expect(
        "index.md should still have a 'Committed files and committability' section \
             — the per-extension pages link to it by this heading's anchor",
    );
    let sentence = paragraph_containing(&index[section_start..], "Ecosystem lock files");
    let lower = sentence.to_lowercase();

    assert!(
        lower.contains("commit") && lower.contains("gitignore"),
        "index.md's ecosystem-lock-files paragraph should name both commit and \
         gitignore as available options, got:\n{sentence}"
    );
    assert!(
        !lower.contains("must be committed") && !lower.contains("should be committed"),
        "index.md's ecosystem-lock-files paragraph should not mandate commit over \
         gitignore (or vice versa), got:\n{sentence}"
    );
}

// ---------------------------------------------------------------------------
// Per-extension pages — defer, don't restate
// ---------------------------------------------------------------------------

/// Phrases that assert a commit verdict in the page's own voice rather than
/// deferring to index.md. Not exhaustive (see module doc "residue" note) —
/// this is the specific decayed phrasing found across all three pages plus
/// its obvious commit/gitignore-mandate variants, not a claim that every
/// possible restatement is covered.
const FLAT_VERDICT_PHRASES: &[&str] = &[
    "committable persistent state",
    "must be committed",
    "should be committed",
    "is not committable",
    "must be gitignored",
    "should be gitignored",
];

fn assert_defers_to_index(doc_path: &str, lock_filename: &str) {
    let doc = read_doc(doc_path);
    let paragraph = paragraph_containing(&doc, lock_filename);
    let lower = paragraph.to_lowercase();

    assert!(
        paragraph.contains(INDEX_ANCHOR),
        "{doc_path}'s paragraph mentioning {lock_filename} should link to \
         {INDEX_ANCHOR} rather than assert a commit policy on its own, got:\n{paragraph}"
    );
    for phrase in FLAT_VERDICT_PHRASES {
        assert!(
            !lower.contains(phrase),
            "{doc_path}'s paragraph about {lock_filename} should defer to index.md \
             instead of asserting its own commit verdict ({phrase:?}), got:\n{paragraph}"
        );
    }
}

#[test]
fn cargo_workspace_md_defers_lock_policy_to_index() {
    assert_defers_to_index(
        "docs/reference/integrations/cargo-workspace.md",
        "Cargo.lock",
    );
}

#[test]
fn npm_workspaces_md_defers_lock_policy_to_index() {
    assert_defers_to_index(
        "docs/reference/integrations/npm-workspaces.md",
        "package-lock.json",
    );
}

#[test]
fn uv_workspace_md_defers_lock_policy_to_index() {
    assert_defers_to_index("docs/reference/integrations/uv-workspace.md", "uv.lock");
}

// ---------------------------------------------------------------------------
// docs/reference/integrations/cargo-workspace.md's artifact table — a cell
// that must say something names the operator's choice, not "Yes"
// ---------------------------------------------------------------------------

#[test]
fn cargo_workspace_md_artifact_table_names_operator_choice_not_yes() {
    let doc = read_doc("docs/reference/integrations/cargo-workspace.md");
    let table_start = doc
        .find("## Three Rust artifacts")
        .expect("cargo-workspace.md should still have the 'Three Rust artifacts' table");
    let table = &doc[table_start..];
    let row_start = table
        .find("| `Cargo.lock` |")
        .expect("the artifact table should still have a Cargo.lock row");
    let row_end = table[row_start..]
        .find('\n')
        .map(|i| row_start + i)
        .unwrap_or(table.len());
    let row = &table[row_start..row_end];

    assert!(
        !row.trim_end().ends_with("| Yes |"),
        "cargo-workspace.md's Cargo.lock artifact-table row should name the \
         operator's choice, not flatly assert committability, got:\n{row}"
    );
    assert!(
        row.contains(INDEX_ANCHOR),
        "cargo-workspace.md's Cargo.lock artifact-table row should point at \
         {INDEX_ANCHOR} for the actual policy, got:\n{row}"
    );
}
