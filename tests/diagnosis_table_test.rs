//! The state-to-choices table prints only what the CLI accepts.
//!
//! A choice whose verb or flag has not shipped is data in the table and is
//! never printed. The prohibition is held where it reaches an operator: every
//! line each state actually prints, judged with every placeholder bound, that
//! spells an `rwv` command must parse through the real clap tree. A second
//! walk over the table itself holds the same for every form marked shipped,
//! `then` forms included, so a form no state offers yet cannot sit shipped
//! and unparseable until the day one does.
//!
//! Residue: parsing proves the spelling exists, not that it means what the
//! choice says. A form marked shipped whose verb exists with another meaning
//! (an `rwv update` that does not yet align members) passes; the `shipped`
//! mark is the judgment that it does not.

use clap::CommandFactory;
use repoweave::cli::Cli;
use repoweave::diagnosis::{Form, Report, State};
use std::collections::BTreeMap;
use std::path::Path;

/// A value for every placeholder a form spells, so a command parses as the
/// operator would run it once filled.
fn samples() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("<project>", "proj"),
        ("<ww>", "seat"),
        ("<m>", "github/o/lib"),
        ("<repo>", "github/o/lib"),
        ("<b>", "main"),
        ("<W>", "feat"),
        ("<T>", "main"),
        ("<rev>", "0123abc"),
        ("<url>", "https://example.com/o/lib.git"),
        ("<role>", "owned"),
        ("<bool>", "true"),
        ("<h>", "linear"),
        ("<new>", "renamed"),
        ("<other>", "other"),
        ("<n>", "1"),
    ])
}

fn filled(command: &str) -> Vec<String> {
    let samples = samples();
    command
        .split_whitespace()
        .map(|word| {
            if word.starts_with('<') {
                (*samples
                    .get(word)
                    .unwrap_or_else(|| panic!("no sample for `{word}` in `{command}`")))
                .to_owned()
            } else {
                word.to_owned()
            }
        })
        .collect()
}

/// Why `command` does not parse as a core verb, or `None` when clap accepts it.
///
/// A word clap does not know as a subcommand parses as an external one — the
/// `rwv-<verb>` plugin fallback — so a parse alone would accept any verb at
/// all. The verb must be one the tree declares.
fn parse_failure(command: &str) -> Option<String> {
    let argv = filled(command);
    assert_eq!(argv[0], "rwv", "`{command}` is not an rwv command");
    let cli = Cli::command();
    if cli.find_subcommand(&argv[1]).is_none() {
        return Some(format!("`{command}`: `{}` is no core verb", argv[1]));
    }
    cli.try_get_matches_from(&argv)
        .err()
        .map(|e| format!("`{command}` → {argv:?}: {e}"))
}

/// `state` judged as refused by `verb`, which adds the forms only that verb
/// offers.
fn judged_with_every_binding(state: State, verb: Option<&'static str>) -> Report {
    let vcs = repoweave::git::git_vcs();
    let mut judging = state
        .of("github/o/lib")
        .checkout(Path::new("/w/github/o/lib"))
        .bind("<primary>", "/w")
        .bind("<worktree>", "/x/wt");
    for (placeholder, value) in samples() {
        judging = judging.bind(placeholder, value);
    }
    if let Some(verb) = verb {
        judging = judging.by_verb(verb);
    }
    let mut report = Report::default();
    report.add(judging.judge(vcs.as_ref()));
    report
}

/// The `rwv` lines `state` prints for `verb`, in `--json` and in the terminal.
fn printed_rwv_lines(state: State, verb: Option<&'static str>) -> Vec<String> {
    let report = judged_with_every_binding(state, verb);
    let mut lines: Vec<String> = report
        .output()
        .into_iter()
        .flat_map(|judged| judged.choices)
        .flat_map(|choice| choice.run)
        .filter(|line| line.starts_with("rwv "))
        .collect();
    let rendered = report.render("verb");
    let terminal: Vec<String> = rendered
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("rwv ") && !line.starts_with("rwv verb:"))
        .filter(|line| !line.starts_with("rwv explain "))
        .map(str::to_owned)
        .collect();
    lines.extend(terminal);
    lines
}

#[test]
fn every_rwv_line_a_state_prints_parses() {
    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for state in State::ALL {
        let verbs = std::iter::once(None)
            .chain(state.row().verb_offers.iter().map(|(verb, _)| Some(*verb)));
        for verb in verbs {
            for line in printed_rwv_lines(state, verb) {
                checked += 1;
                if let Some(failure) = parse_failure(&line) {
                    failures.push(format!("{state:?} as {verb:?}: {failure}"));
                }
            }
        }
    }
    assert!(
        checked >= 4,
        "only {checked} printed rwv lines were read; the walk has stopped reaching the table"
    );
    assert!(
        failures.is_empty(),
        "these states print a command the CLI does not accept — a choice prints only once \
         its verb ships:\n{failures:#?}"
    );
}

#[test]
fn every_shipped_form_spells_only_commands_that_parse() {
    let shipped: Vec<Form> = Form::ALL.into_iter().filter(|f| f.printable()).collect();
    assert!(
        shipped.iter().any(|f| !f.rwv_commands().is_empty()),
        "no printable form spells an rwv command; the walk would hold over nothing"
    );
    let failures: Vec<String> = shipped
        .iter()
        .flat_map(|form| {
            form.rwv_commands()
                .into_iter()
                .filter_map(move |command| parse_failure(command).map(|f| format!("{form:?}: {f}")))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "these forms are marked shipped but spell commands clap rejects:\n{failures:#?}"
    );
}

/// The seeded failure for both walks: a command the CLI has not shipped is
/// reported, and so is one with a flag it does not take.
#[test]
fn an_unshipped_spelling_is_reported() {
    assert!(parse_failure("rwv member set-branch <m> <b>").is_some());
    assert!(parse_failure("rwv branch rename <new>").is_none());
    assert!(parse_failure("rwv branch rename <new> --adopt").is_none());
    assert!(parse_failure("rwv branch rename <new> --keep").is_some());
    assert!(parse_failure("rwv workweave delete <ww> --discard-unmerged-commits").is_some());
    assert!(parse_failure("rwv push --repo <m>").is_none());
    assert!(parse_failure("rwv push --branch <W>").is_none());
    assert!(parse_failure("rwv workweave delete <ww>").is_none());
    assert!(!Form::C16Switch.printable());
    assert!(
        Form::C16Switch
            .rwv_commands()
            .iter()
            .any(|c| parse_failure(c).is_some()),
        "the fixture form must really be unshipped, or the walks above prove nothing about it"
    );
}

/// Every form a state offers belongs to a choice the table defines, and the
/// `--json` record of each printed choice carries the id the page lists.
#[test]
fn a_printed_choice_carries_the_id_of_the_form_it_came_from() {
    for state in State::ALL {
        let expected: Vec<Option<&str>> = state
            .row()
            .offers
            .iter()
            .filter(|f| f.printable())
            .map(|f| f.row().choice.row().id)
            .collect();
        let report = judged_with_every_binding(state, None);
        let printed: Vec<Option<String>> = report.output()[0]
            .choices
            .iter()
            .map(|c| c.id.clone())
            .collect();
        assert_eq!(
            printed,
            expected
                .iter()
                .map(|id| id.map(str::to_owned))
                .collect::<Vec<_>>(),
            "{state:?}"
        );
    }
}

/// A form only one verb's flag spells is offered to that verb's refusal and to
/// no other: a dirty `sync` is not offered a `workweave delete`.
#[test]
fn a_verbs_own_form_prints_only_for_that_verb() {
    let delete_form = "rwv workweave delete seat --discard-uncommitted";
    let own = printed_rwv_lines(State::EditsDirty, Some("workweave delete"));
    assert!(
        own.iter().any(|line| line == delete_form),
        "the verb's own form must print for it: {own:?}"
    );
    for verb in [None, Some("sync"), Some("sync-to")] {
        let others = printed_rwv_lines(State::EditsDirty, verb);
        assert!(
            !others.iter().any(|line| line.contains("workweave delete")),
            "{verb:?} must not be offered another verb's form: {others:?}"
        );
    }
}

/// Every verb that a state names for its own forms is a verb the CLI has.
#[test]
fn a_verb_named_for_its_own_forms_is_in_the_cli() {
    let cli = Cli::command();
    let mut named = 0;
    for state in State::ALL {
        for (verb, _) in state.row().verb_offers {
            named += 1;
            let mut at = &cli;
            for word in verb.split(' ') {
                at = at
                    .find_subcommand(word)
                    .unwrap_or_else(|| panic!("`{verb}` is no verb of the CLI (at `{word}`)"));
            }
        }
    }
    assert!(
        named > 0,
        "no state names a verb; the walk holds over nothing"
    );
}

/// A line judged for an activation sends the operator to the spelling that
/// aligns before it activates, wherever the table would print `rwv update`:
/// as the choice, and as the step after keeping work on a branch. A run in
/// place keeps `rwv update`.
#[test]
fn an_alignment_line_judged_for_activation_never_prints_rwv_update() {
    let states = [
        State::AlignmentOnBranch,
        State::AlignmentOnBranchWithStrayCommits,
        State::AlignmentDetachedElsewhere,
        State::AlignmentDetachedUnpushed,
    ];
    let mut aligning = 0usize;
    for state in states {
        let activation = printed_rwv_lines(state, Some("activate"));
        let in_place = printed_rwv_lines(state, None);
        assert!(
            !activation.iter().any(|line| line == "rwv update"),
            "{state:?} judged for activation prints rwv update: {activation:?}"
        );
        for line in activation.iter().filter(|line| line.contains("--update")) {
            aligning += 1;
            assert_eq!(line, "rwv activate proj --update", "{state:?}");
            assert_eq!(parse_failure(line), None, "{state:?}");
        }
        assert!(
            in_place.iter().all(|line| !line.contains("--update")),
            "{state:?} judged in place prints the activation spelling: {in_place:?}"
        );
    }
    assert!(
        aligning >= states.len(),
        "only {aligning} activation spellings were read over {} states; the walk reaches nothing",
        states.len()
    );
}
