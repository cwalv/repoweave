//! Pins the on-disk spelling of `op_state::Override` to the CLI flags it
//! records.
//!
//! The `overrides` list in `.rwv-op` is the audit trail of which consent flags
//! an op ran under. `--continue` re-derives the op's consent from it and
//! `cleanup` reads it to decide whether the project savepoint survives as the
//! only remaining pointer to discarded commits, so a record written by an
//! earlier rwv has to keep parsing, and the spelling has to stay the flag's
//! own name.
//!
//! Neither side of the comparison is typed into this file: the expected
//! spellings come out of `Cli::command()`, the actual ones out of serialising
//! the enum.

use clap::CommandFactory;
use repoweave::cli::Cli;
use repoweave::op_state::{self, Override};

mod common;

/// Every `Override`, with a match that stops compiling when a variant is
/// added — the list below is what the assertions iterate, so it has to grow
/// with the enum.
fn all_overrides() -> Vec<Override> {
    let all = vec![Override::AllowStaleLock, Override::DiscardLocalCommits];
    for o in &all {
        match o {
            Override::AllowStaleLock | Override::DiscardLocalCommits => {}
        }
    }
    all
}

/// The `Consent:` prefix every override flag's help opens with. Read off the
/// flags themselves below rather than restated, so this is only the marker,
/// not a copy of the vocabulary.
const CONSENT_MARKER: &str = "Consent:";

/// Long-flag names of every `Consent:`-marked flag on `<subcommand>`.
fn consent_flag_names(subcommand: &str) -> Vec<String> {
    let cmd = Cli::command();
    let sub = cmd
        .get_subcommands()
        .find(|s| s.get_name() == subcommand)
        .unwrap_or_else(|| panic!("no `{subcommand}` subcommand in the CLI tree"));
    let mut names: Vec<String> = sub
        .get_arguments()
        .filter(|a| {
            a.get_help()
                .is_some_and(|h| h.to_string().starts_with(CONSENT_MARKER))
        })
        .filter_map(|a| a.get_long().map(str::to_owned))
        .collect();
    names.sort();
    names
}

fn wire_spelling(o: &Override) -> String {
    serde_json::to_string(o)
        .expect("Override serialises")
        .trim_matches('"')
        .to_owned()
}

fn sorted_wire_spellings() -> Vec<String> {
    let mut s: Vec<String> = all_overrides().iter().map(wire_spelling).collect();
    s.sort();
    s
}

#[test]
fn every_override_spells_the_consent_flag_that_mints_it() {
    let flags = consent_flag_names("sync");
    assert!(
        flags.len() >= 2,
        "found {} consent flags on `sync` — the help-text scan matched nothing \
         useful, so the comparison below would pass vacuously; got {flags:?}",
        flags.len()
    );
    assert_eq!(
        consent_flag_names("sync-to"),
        flags,
        "`sync` and `sync-to` record the same consent vocabulary"
    );
    assert_eq!(
        sorted_wire_spellings(),
        flags,
        "each Override must serialise to its flag's name with the dashes \
         stripped, and each consent flag must have an Override to record it"
    );
}

#[test]
fn an_owner_record_naming_every_consent_flag_parses() {
    let dir = common::tempdir().expect("tempdir");
    let list: String = consent_flag_names("sync-to")
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let json = format!(
        "{{\"id\": \"1779769917405921588\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"/src\", \"target\": \"/tgt\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [{list}], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}"
    );
    std::fs::write(dir.path().join(".rwv-op"), &json).expect("write record");

    let record = op_state::read_owner(dir.path())
        .expect("a record naming the consent flags must parse")
        .expect("record present");

    let mut got: Vec<String> = record.overrides.iter().map(wire_spelling).collect();
    got.sort();
    assert_eq!(
        got,
        sorted_wire_spellings(),
        "an on-disk record naming every consent flag must read back as every \
         Override; the JSON under test was:\n{json}"
    );
}
