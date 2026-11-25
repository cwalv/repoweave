//! `rwv branch set-history`: write or remove the checked-out weave branch's
//! `history`, the top-level one or a member's, and commit only that change.

use crate::manifest::{History, Manifest, ProjectName, RepoOperand, RepoPath};
use crate::member_branch::Loaded;
use crate::refusal::RefusalKind;
use crate::workspace::WorkspaceContext;

pub const VERB: &str = "branch set-history";

/// What `set-history` writes: a value, or none (`--unset`), at the top level
/// or for one member.
#[derive(Debug, Clone)]
pub struct SetHistory {
    pub history: Option<History>,
    pub repo: Option<RepoOperand>,
}

/// Which `history` of a manifest a write names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HistoryKey {
    TopLevel(ProjectName),
    Member(RepoPath),
}

impl HistoryKey {
    pub(crate) fn read(&self, manifest: &Manifest) -> Option<History> {
        match self {
            Self::TopLevel(_) => manifest.history,
            Self::Member(member) => manifest.get_entry(member).and_then(|entry| entry.history),
        }
    }

    pub(crate) fn write(&self, manifest: &mut Manifest, history: Option<History>) {
        match self {
            Self::TopLevel(_) => manifest.history = history,
            Self::Member(member) => {
                if let Some(mut entry) = manifest.get_entry(member).cloned() {
                    entry.history = history;
                    manifest.insert_repo(member.clone(), entry);
                }
            }
        }
    }
}

impl std::fmt::Display for HistoryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TopLevel(project) => {
                write!(f, "{}", RepoOperand::Project(project.clone()))
            }
            Self::Member(member) => f.write_str(member.as_str()),
        }
    }
}

/// A `history` value as messages print it.
pub(crate) fn spelled(history: Option<History>) -> String {
    history.map_or_else(|| "unset".to_owned(), |h| h.to_string())
}

/// The line a change of `key` from `was` to `now` prints.
pub(crate) fn change_line(
    verb: &str,
    key: &HistoryKey,
    was: Option<History>,
    now: Option<History>,
) -> String {
    format!(
        "rwv {verb}: {key}: history {} -> {}",
        spelled(was),
        spelled(now)
    )
}

fn key_of(
    project: &ProjectName,
    manifest: &Manifest,
    repo: Option<&RepoOperand>,
) -> anyhow::Result<HistoryKey> {
    match repo {
        None => Ok(HistoryKey::TopLevel(project.clone())),
        Some(RepoOperand::Project(named)) if named == project => {
            Ok(HistoryKey::TopLevel(project.clone()))
        }
        Some(operand @ RepoOperand::Project(named)) => Err(crate::untokened!(
            Owed,
            "`{operand}` is the repo of project `{named}`, not of `{project}`; name this \
             project's top-level setting as `{}`, or run with `--project {named}`",
            RepoOperand::Project(project.clone())
        )),
        Some(RepoOperand::Member(member)) => {
            if !manifest.contains_repo(member) {
                crate::refuse!(
                    RefusalKind::RepoNotInManifest,
                    "path '{member}' not found in manifest"
                );
            }
            Ok(HistoryKey::Member(member.clone()))
        }
    }
}

/// Execute `rwv branch set-history (<h> | --unset) [--repo <m>]`.
///
/// Every refusal is reported before anything is written. The working
/// `rwv.toml` takes the change beside whatever else it holds, and one commit
/// carries HEAD's file with only this change, so a value a hand edit already
/// wrote is committed by re-running this.
pub fn run(ctx: &WorkspaceContext, request: SetHistory) -> anyhow::Result<()> {
    let loaded = Loaded::open(ctx, VERB)?;
    let key = key_of(&loaded.project, &loaded.manifest, request.repo.as_ref())?;
    let authored =
        crate::manifest_commit::authored_across(ctx, &loaded.project, &[&loaded.manifest]);
    crate::manifest_commit::judge(&loaded.project_dir)?;

    let was = key.read(&loaded.manifest);
    let now = request.history;
    let mut manifest = loaded.manifest.clone();
    key.write(&mut manifest, now);
    crate::add_remove::settle_manifest(
        ctx,
        &loaded.project,
        (was != now).then_some(&manifest),
        &[],
        crate::activate::ActivateOptions {
            hooks_never_run: true,
            ..Default::default()
        },
    )?;
    let committed_was = crate::manifest_commit::committed_manifest(
        crate::vcs::project_vcs().as_ref(),
        &loaded.project_dir,
    )?
    .and_then(|head| key.read(&head));
    let edit = {
        let key = key.clone();
        move |manifest: &mut Manifest| key.write(manifest, now)
    };
    let committed = crate::manifest_commit::commit(&crate::manifest_commit::MemberCommit {
        project: &loaded.project,
        project_dir: &loaded.project_dir,
        edit_manifest: &edit,
        edit_lock: None,
        authored: &authored,
        message: &format!(
            "branch set-history {key} {}\n\n  - {key}: history {} -> {}",
            spelled(now),
            spelled(committed_was),
            spelled(now)
        ),
        naming: crate::repin::Naming::Unnamed,
    })?;
    if was == now && !committed {
        eprintln!(
            "rwv {VERB}: {key}: history is already {}; nothing to change",
            spelled(now)
        );
    } else {
        eprintln!("{}", change_line(VERB, &key, committed_was, now));
    }
    Ok(())
}

/// How a repo takes in or lands work a fast-forward does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Integration {
    /// Its commits are replayed onto the side they integrate with.
    Rebase,
    /// A merge commit joins the two.
    Merge,
}

/// The refusal for `--history` against a value the target records, whether or
/// not the run needs the setting.
pub(crate) fn contradicted(
    recorded: Option<History>,
    one_run: Option<History>,
    member: bool,
    scoped: impl Fn(crate::diagnosis::State) -> crate::diagnosis::Judging,
    vcs: &dyn crate::vcs::Vcs,
) -> Option<crate::diagnosis::Judged> {
    use crate::diagnosis::{Form, State};
    let (Some(recorded), Some(one_run)) = (recorded, one_run) else {
        return None;
    };
    if recorded == one_run {
        return None;
    }
    let set = if member {
        Form::C23SetHistoryRepo
    } else {
        Form::C23SetHistory
    };
    Some(
        scoped(State::HistoryContradicted)
            .on_target(vcs)
            .subject(format!(
                "`--history {one_run}` for one run, where the target records {recorded}"
            ))
            .only_forms(&[set, Form::KeepRecordedHistory])
            .judge(vcs),
    )
}

/// The setting that governs one repo of a `sync` or `land` where a
/// fast-forward does not do: the target's own, else `--history` for one run.
/// `--history` stands in only where the target records nothing; a value
/// differing from a recorded one is refused.
pub(crate) fn governing(
    recorded: Option<History>,
    one_run: Option<History>,
    member: bool,
    scoped: impl Fn(crate::diagnosis::State) -> crate::diagnosis::Judging,
    vcs: &dyn crate::vcs::Vcs,
) -> Result<History, Box<crate::diagnosis::Judged>> {
    use crate::diagnosis::{Form, State};
    if let Some(judged) = contradicted(recorded, one_run, member, &scoped, vcs) {
        return Err(Box::new(judged));
    }
    let set = if member {
        Form::C23SetHistoryRepo
    } else {
        Form::C23SetHistory
    };
    match (recorded, one_run) {
        (Some(recorded), _) => Ok(recorded),
        (None, Some(one_run)) => Ok(one_run),
        (None, None) => Err(Box::new(
            scoped(State::HistoryUnset)
                .on_target(vcs)
                .subject("the target records no history for it")
                .only_forms(&[Form::C23SetHistory, set, Form::C23Rerun])
                .judge(vcs),
        )),
    }
}

/// The first merge commit among the commits `tip` holds that `onto` lacks:
/// the commits a rebase onto `onto` would replay.
pub(crate) fn merge_in_range(
    vcs: &dyn crate::vcs::Vcs,
    dir: &std::path::Path,
    tip: &crate::vcs::CommitId,
    onto: &crate::vcs::CommitId,
) -> Result<Option<crate::vcs::CommitId>, crate::vcs::VcsError> {
    let rev = |c: &crate::vcs::CommitId| crate::vcs::ResolvedRevisionId::from(c.clone());
    let range = vcs.pushed_range(dir, &rev(tip), Some(&rev(onto)))?;
    Ok(range
        .into_iter()
        .find(|c| c.parents.len() > 1)
        .map(|c| c.id))
}

/// How a repo whose work a fast-forward does not integrate is integrated
/// under `history`: a linear-family setting rebases, unless the commits a
/// rebase would replay hold a merge, which a rebase would flatten; those are
/// integrated by a merge commit, the merge returned so the run reports it.
pub(crate) fn integration(
    history: History,
    merge_in_range: Option<crate::vcs::CommitId>,
) -> (Integration, Option<crate::vcs::CommitId>) {
    match (history, merge_in_range) {
        (History::Merge, _) => (Integration::Merge, None),
        (History::Linear | History::FfOnly, Some(merge)) => (Integration::Merge, Some(merge)),
        (History::Linear | History::FfOnly, None) => (Integration::Rebase, None),
    }
}

/// The `merged` report line for a repo whose range holding `merge` is
/// integrated by a merge commit under a rebasing setting.
pub(crate) fn merged_line(repo: &dyn std::fmt::Display, merge: &crate::vcs::CommitId) -> String {
    format!(
        "{repo}: merged: the commits a rebase would replay hold the merge {}, which a rebase \
         would flatten, so they are integrated by a merge commit",
        merge.short()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::git_vcs;

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn tip(dir: &std::path::Path) -> crate::vcs::CommitId {
        crate::vcs::CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap()
    }

    #[test]
    fn a_range_with_no_merge_base_holds_its_merge_though_every_commit_is_pushed() {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-b", "main"]);
        for (key, value) in [("user.email", "t@t"), ("user.name", "T")] {
            git(repo, &["config", key, value]);
        }
        git(repo, &["commit", "--allow-empty", "-m", "main"]);
        let onto = tip(repo);
        git(repo, &["checkout", "--orphan", "unrelated"]);
        git(repo, &["commit", "--allow-empty", "-m", "u1"]);
        git(repo, &["checkout", "-b", "side"]);
        git(repo, &["commit", "--allow-empty", "-m", "side"]);
        git(repo, &["checkout", "unrelated"]);
        git(repo, &["merge", "--no-ff", "side", "-m", "merge"]);
        let merge = tip(repo);
        git(
            repo,
            &[
                "update-ref",
                "refs/remotes/origin/unrelated",
                merge.as_str(),
            ],
        );

        let found = merge_in_range(git_vcs().as_ref(), repo, &merge, &onto).unwrap();

        assert_eq!(found, Some(merge));
    }
}
