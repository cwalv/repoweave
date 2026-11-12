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
            no_materialize: true,
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
