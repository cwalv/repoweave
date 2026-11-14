//! Which verbs run while an operation is recorded in, or leases, the
//! workspace they act on. Every other verb refuses there before it reads or
//! writes anything, and a verb this table does not name is refused.

use std::path::Path;

/// What a verb may do in a workspace an operation holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuringAnOperation {
    /// It reads, it is the operation's own `--continue` or `abort`, or it
    /// repairs only what leaves the operation's savepoints and record as
    /// they stand.
    Runs,
    Refused,
}

/// The flags whose presence changes whether a verb runs during an
/// operation, by clap id, each with its spelling.
const DECIDING_FLAGS: [(&str, &str); 3] = [
    ("do_continue", "--continue"),
    ("fix", "--fix"),
    ("merge_driver", "--merge-driver"),
];

/// A program on `PATH` that `rwv` hands the invocation to.
const PLUGIN: &str = "<plugin>";

/// The verbs that run during an operation, spelled as `spelling` spells them.
const RUN: &[&str] = &[
    "",
    "abort",
    "backup list",
    "branch list",
    "cherry-pick --continue",
    "completions",
    "doctor",
    "doctor --fix",
    "explain",
    "land --continue",
    "lock --merge-driver",
    "log",
    "prime",
    "pull --continue",
    "resolve",
    "setup claude",
    "status",
    "sync --continue",
    "update --continue",
    "workweave list",
    PLUGIN,
];

/// The verbs decided as refused. [`during_an_operation`] refuses these and
/// any verb no list names alike; the list is what a new verb is held to.
const REFUSED: &[&str] = &[
    "activate",
    "backup drop",
    "backup prune",
    "branch create",
    "branch delete",
    "branch rename",
    "branch set-history",
    "branch set-mode",
    "cherry-pick",
    "clone",
    "fetch",
    "init",
    "land",
    "lock",
    "materialize",
    "member add",
    "member pin",
    "member remove",
    "member set-branch",
    "member set-recurse-submodules",
    "member set-role",
    "member set-url",
    "pull",
    "push",
    "switch",
    "sync",
    "update",
    "workweave",
    "workweave create",
    "workweave delete",
    "workweave set-default-create-basedir",
];

/// Every spelling a list decides, with its decision.
pub fn decided() -> impl Iterator<Item = (&'static str, DuringAnOperation)> {
    let listed = |list: &'static [&'static str], decision| list.iter().map(move |s| (*s, decision));
    listed(RUN, DuringAnOperation::Runs).chain(listed(REFUSED, DuringAnOperation::Refused))
}

/// The invocation as this table spells it: its subcommand path, then each
/// deciding flag given on the command line.
pub(crate) fn spelling(command: &clap::Command, matches: &clap::ArgMatches) -> String {
    let mut words: Vec<String> = Vec::new();
    let (mut command, mut at) = (command, matches);
    while let Some((name, sub)) = at.subcommand() {
        let Some(known) = command.find_subcommand(name) else {
            return PLUGIN.to_owned();
        };
        words.push(name.to_owned());
        command = known;
        at = sub;
    }
    for (id, flag) in DECIDING_FLAGS {
        let given = command.get_arguments().any(|arg| arg.get_id() == id)
            && at.value_source(id) == Some(clap::parser::ValueSource::CommandLine);
        if given {
            words.push(flag.to_owned());
        }
    }
    words.join(" ")
}

pub(crate) fn during_an_operation(spelling: &str) -> DuringAnOperation {
    if RUN.contains(&spelling) {
        DuringAnOperation::Runs
    } else {
        DuringAnOperation::Refused
    }
}

/// Refuse `spelling` where an operation holds `workspace`, as
/// [`during_an_operation`] decides. A verb that commits the project repo,
/// where the operation stopped in it, is refused as that commit would be,
/// naming the continues that settle the stop.
pub(crate) fn refuse_where_held(
    spelling: &str,
    workspace: Option<&Path>,
    commits_project: bool,
) -> anyhow::Result<()> {
    let Some(workspace) = workspace else {
        return Ok(());
    };
    match during_an_operation(spelling) {
        DuringAnOperation::Runs => Ok(()),
        DuringAnOperation::Refused => {
            if commits_project {
                refuse_a_commit_at_the_stop(workspace)?;
            }
            crate::op_state::check_no_op_in_progress(&[workspace])
        }
    }
}

fn refuse_a_commit_at_the_stop(workspace: &Path) -> anyhow::Result<()> {
    let Some(header) = crate::op_state::read_owner_header(workspace).ok().flatten() else {
        return Ok(());
    };
    let dir = crate::workspace::project_dir(workspace, &header.project);
    crate::commit_guard::refuse_commit_mid_operation(
        crate::vcs::project_vcs().as_ref(),
        crate::workspace::project_rel_path(&header.project),
        &dir,
        Some(workspace),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn spellings_of(command: &clap::Command, path: &[&str], into: &mut Vec<String>) {
        let flags: Vec<&str> = DECIDING_FLAGS
            .iter()
            .filter(|(id, _)| command.get_arguments().any(|arg| arg.get_id() == *id))
            .map(|(_, flag)| *flag)
            .collect();
        if command.get_subcommands().next().is_none() || !command.is_subcommand_required_set() {
            into.push(path.join(" "));
        }
        for flag in flags {
            into.push(
                path.iter()
                    .copied()
                    .chain([flag])
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        for sub in command.get_subcommands() {
            let mut deeper = path.to_vec();
            deeper.push(sub.get_name());
            spellings_of(sub, &deeper, into);
        }
    }

    fn derived() -> Vec<String> {
        let mut spellings = vec![PLUGIN.to_owned()];
        spellings_of(&crate::cli::Cli::command(), &[], &mut spellings);
        spellings
    }

    /// Every spelling clap's tree yields is decided by exactly one list, and
    /// every listed spelling is one the tree yields: a verb added to the CLI
    /// fails here until it is listed, and runs refused until then.
    #[test]
    fn every_verb_is_listed_once_as_running_or_refused_during_an_operation() {
        let derived = derived();
        assert!(derived.iter().any(|s| s == "member set-role"));
        assert!(derived.iter().any(|s| s == "land --continue"));
        let listed: Vec<&str> = decided().map(|(spelling, _)| spelling).collect();
        let unlisted: Vec<&String> = derived
            .iter()
            .filter(|s| !listed.contains(&s.as_str()))
            .collect();
        assert!(
            unlisted.is_empty(),
            "verbs neither run nor refused during an operation: {unlisted:?}"
        );
        let stale: Vec<&&str> = listed
            .iter()
            .filter(|s| !derived.iter().any(|d| d == *s))
            .collect();
        assert!(stale.is_empty(), "listed spellings no verb has: {stale:?}");
        let twice: Vec<&&str> = listed
            .iter()
            .filter(|s| listed.iter().filter(|t| t == s).count() > 1)
            .collect();
        assert!(twice.is_empty(), "listed more than once: {twice:?}");
    }

    #[test]
    fn an_unlisted_verb_is_refused() {
        assert_eq!(
            during_an_operation("member frobnicate"),
            DuringAnOperation::Refused
        );
    }

    #[test]
    fn spelling_reads_the_path_and_deciding_flags() {
        let command = crate::cli::Cli::command();
        let spelled = |argv: &[&str]| {
            let matches = command.clone().try_get_matches_from(argv).unwrap();
            spelling(&command, &matches)
        };
        assert_eq!(spelled(&["rwv", "land", "--continue"]), "land --continue");
        assert_eq!(spelled(&["rwv", "land"]), "land");
        assert_eq!(
            spelled(&["rwv", "member", "set-role", "a", "reference"]),
            "member set-role"
        );
        assert_eq!(spelled(&["rwv", "doctor", "--fix"]), "doctor --fix");
        assert_eq!(spelled(&["rwv", "rwv-graph-xyz", "--x"]), PLUGIN);
        assert_eq!(spelled(&["rwv"]), "");
    }
}
