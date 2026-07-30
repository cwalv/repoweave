//! The command that resumes an interrupted op is minted in one place.
//!
//! `op_state.rs` answers for it: `resume_command` derives the text from the
//! op's own recorded verb, which is the whole point — a resumed op reads every
//! parameter back out of op-state, so the operator is told to rerun the verb
//! that started it and nothing else. Five callers had written the string out
//! by hand instead, three of them naming a verb they had decided on rather
//! than read.
//!
//! There is no closed literal to match: the verb is interpolated, so the mint
//! and every hand-written copy differ in the middle. The needle is the shape —
//! `--continue` as a flag, whose immediately preceding token is preceded by
//! `rwv `. That admits the mint's `rwv {verb} --continue` and a caller's
//! `rwv sync-to --continue` alike, and it discriminates against the two shapes
//! that must survive: git's own `git rebase --continue`, which rwv's conflict
//! text names to tell the operator NOT to run it, and a bare `` `--continue` ``
//! in prose about the flag.
//!
//! Doc comments and `#[cfg(test)]` bodies are exempt, and deliberately.
//! `src_scan` drops both: a doc comment quoting the resume text is prose about
//! the contract, and an assertion spelling the expected output is the test
//! doing its job — an assertion that called the mint instead would agree with
//! it by construction and could never catch it changing.
//!
//! Residue, for anyone extending this. The scan is line-oriented, so a copy
//! split across a `\`-continued string boundary such that `rwv ` and the flag
//! land on different lines reads as absent. It says nothing about a caller
//! that spells the verb but leaves the flag to a variable, nor about
//! `rwv abort`, the other exit every one of these messages offers and which
//! has no mint at all. It inherits `src_scan`'s line-leading `//` comment
//! filter, and it says nothing about `tests/`, an external crate where
//! fixtures assert on the rendered text by design.

mod common;

use common::src_scan::{production_lines, SourceLine};

const RESUME_FLAG: &str = " --continue";
const VERB_PREFIX: &str = "rwv ";
const OWNER: &str = "fn resume_command(";

/// True when `text` spells a resume command: the flag, preceded by a single
/// whitespace-free token, preceded by the program name.
fn spells_resume_command(text: &str) -> bool {
    text.match_indices(RESUME_FLAG).any(|(at, _)| {
        text[..at]
            .rsplit_once(VERB_PREFIX)
            .is_some_and(|(_, verb)| !verb.is_empty() && !verb.contains(char::is_whitespace))
    })
}

fn sites(lines: &[SourceLine]) -> Vec<&SourceLine> {
    lines
        .iter()
        .filter(|l| spells_resume_command(&l.text))
        .collect()
}

#[test]
fn the_scan_is_pointed_at_a_whole_source_tree() {
    let lines = production_lines();
    assert!(
        lines.len() >= 10_000,
        "expected at least 10000 production lines under src/, got {} — a \
         one-site result below would be measuring the corpus, not the source",
        lines.len()
    );
}

#[test]
fn the_resume_command_is_spelled_once_and_at_its_mint() {
    let lines = production_lines();
    let found = sites(&lines);

    assert_eq!(
        found.len(),
        1,
        "the resume command must be written at exactly one production site. A \
         second one is a caller composing text `op_state::resume_command` \
         already derives, and a caller that names the verb itself has decided \
         it rather than read it off the op. Found: {:?}",
        found.iter().map(|l| l.site()).collect::<Vec<_>>()
    );

    let site = found[0];
    assert_eq!(
        site.file,
        "op_state.rs",
        "the one site must be op_state.rs, where the op's verb is; found {}",
        site.site()
    );
    assert!(
        site.text.contains("format!"),
        "the surviving site is a message rather than the mint, so this file's \
         one-site count no longer says what it claims: {}",
        site.text.trim()
    );
    assert!(
        lines
            .iter()
            .any(|l| l.file == "op_state.rs" && l.text.contains(OWNER)),
        "`resume_command` is gone from op_state.rs — the one site above is a \
         mint nothing calls"
    );
}

#[test]
fn a_hand_written_resume_command_is_what_this_reports() {
    let planted = |file: &str, text: &str| SourceLine {
        file: file.to_string(),
        line: 1,
        text: text.to_string(),
    };
    let corpus = vec![
        planted("op_state.rs", "    format!(\"rwv {verb} --continue\")"),
        planted(
            "sync.rs",
            "             Rerun `rwv sync-to --continue` after resolving.\",",
        ),
        planted(
            "check.rs",
            "     `git rebase --continue`; run `rwv doctor --fix` to plant)",
        ),
        planted(
            "op_state.rs",
            "                 If you meant to start a new op, omit `--continue`.\",",
        ),
    ];

    let found = sites(&corpus);
    assert_eq!(
        found.len(),
        2,
        "the seeded caller must be reported alongside the mint; got {:?}",
        found.iter().map(|l| l.text.trim()).collect::<Vec<_>>()
    );
    assert!(
        found.iter().any(|l| l.file == "sync.rs"),
        "the seeded caller is the one this scan exists for and it went unseen"
    );
    assert!(
        found.iter().all(|l| l.file != "check.rs"),
        "a line naming git's own continue flag alongside an unrelated rwv \
         command must not register — that text exists to steer the operator \
         away from the git command, and reporting it would get this scan \
         turned off"
    );
    assert!(
        found.iter().all(|l| !l.text.contains("omit")),
        "prose about the flag with no verb in front of it must not register"
    );
}
