//! A serde derive in production source spells its wire tokens kebab-case or
//! says why it does not, and `snake_case` is never the spelling: the house's
//! wire tokens are kebab-case, so a multi-word variant renamed that way would
//! come out as `two_words` beside every `two-words` around it.
//!
//! Scope: production lines under `src/` outside `#[cfg(test)]` and comments,
//! read for a `rename_all` attribute. A rename written another way, or a type
//! with a hand-written `Serialize`, is invisible to this scan.

mod common;

use common::src_scan::production_lines;

#[test]
fn no_serde_derive_spells_its_tokens_snake_case() {
    let lines = production_lines();

    let kebab = lines
        .iter()
        .filter(|l| l.text.contains("rename_all = \"kebab-case\""))
        .count();
    assert!(
        kebab > 10,
        "{kebab} kebab-case derives found — the needle no longer matches the source shape, so \
         emptiness below would prove nothing"
    );

    let hits: Vec<String> = lines
        .iter()
        .filter(|l| l.text.contains("rename_all = \"snake_case\""))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(hits.is_empty(), "snake_case wire tokens: {hits:#?}");
}
