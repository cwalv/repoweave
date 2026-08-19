//! Pins the prohibition this test enforces: `--` is not spelled as
//! the `<project>--<name>` workweave separator anywhere outside
//! `naming.rs`. Everywhere else routes through `join_flat`,
//! `weave_dir_name`, `split_at_weave_separator`, `workweave_name_in`, or
//! `resolve_flat_address`.
//!
//! The `+` half of the same grammar is pinned by
//! `tests/segment_escape_one_owner_test.rs`. Both metacharacters have the
//! same owner, and the flat address is a bijection only while both hold.
//!
//! The needles are the two shapes that spell the separator directly: a
//! format string joining two placeholders with a bare `--` (the mint shape,
//! `}--{`) and `split_once("--")` (the split shape, which
//! `split_at_weave_separator` is itself built on). A bare substring scan for
//! `"--"` is unusable here: `check.rs`, `git.rs`, `lock.rs`, and `plugins.rs`
//! all pass the literal `--` as a positional git/shell-CLI argument
//! separator, unrelated to workweave naming, and those hits would drown any
//! real violation.
//!
//! The format-join needle has one further false positive to guard against:
//! `{{`/`}}` is how a format string escapes a literal brace, so an
//! operator-facing message that prints the pre-flat `{project}--{workweave}`
//! shape *literally* (`workweave.rs`'s occupied-namespace `bail!`) spells it
//! `{{project}}--{{workweave}}`, which also contains `}--{` as a substring.
//! [`is_format_join`] excludes a match flanked by the extra brace on both
//! sides, which a real placeholder join never is.

mod common;

use common::src_scan::production_lines;

const OWNER: &str = "naming.rs";
const SPLIT_NEEDLE: &str = "split_once(\"--\")";

/// True when `text` contains a bare `--` joining two format placeholders
/// (`}--{`), excluding the `{{`/`}}`-escaped-brace false positive described
/// above.
fn is_format_join(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(rel) = text[start..].find("}--{") {
        let at = start + rel;
        let escaped = at > 0 && bytes[at - 1] == b'}' && bytes.get(at + 4) == Some(&b'{');
        if !escaped {
            return true;
        }
        start = at + 1;
    }
    false
}

#[test]
fn is_format_join_ignores_escaped_braces() {
    assert!(is_format_join("{project_name}--{workweave_name}"));
    assert!(!is_format_join("{{project}}--{{workweave}}"));
}

#[test]
fn naming_rs_still_mints_the_needles_this_scan_looks_for() {
    let lines = production_lines();
    let owner_lines: Vec<_> = lines.iter().filter(|l| l.file == OWNER).collect();

    assert!(
        owner_lines.iter().any(|l| is_format_join(&l.text)),
        "expected the `}}--{{` mint shape in src/{OWNER} and found none — \
         the needle no longer matches the source shape, so an empty result \
         under the rest of src/ would prove nothing"
    );
    assert!(
        owner_lines.iter().any(|l| l.text.contains(SPLIT_NEEDLE)),
        "expected `{SPLIT_NEEDLE}` in src/{OWNER} and found none — the \
         needle no longer matches the source shape, so an empty result \
         under the rest of src/ would prove nothing"
    );
}

#[test]
fn no_module_outside_the_owner_spells_the_weave_separator() {
    let lines = production_lines();
    assert!(
        lines.len() >= 20_000,
        "expected at least 20,000 production lines under src/, got {} — \
         this scan is pointed at the wrong corpus",
        lines.len()
    );

    let outside: Vec<_> = lines.iter().filter(|l| l.file != OWNER).collect();

    let format_hits: Vec<String> = outside
        .iter()
        .filter(|l| is_format_join(&l.text))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    let split_hits: Vec<String> = outside
        .iter()
        .filter(|l| l.text.contains(SPLIT_NEEDLE))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        format_hits.is_empty() && split_hits.is_empty(),
        "the `<project>--<name>` workweave separator must be spelled only \
         in src/{OWNER} — mint via join_flat or weave_dir_name, split via \
         split_at_weave_separator, and read an identity back via \
         workweave_name_in or resolve_flat_address. \
         format-join hits: {format_hits:#?}, split hits: {split_hits:#?}"
    );
}
