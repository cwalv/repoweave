//! The members two projects list, and what a verb for one of them may do to
//! those members while the weave root presents the other.
//!
//! A member has one checkout. A project that is not the presented one has no
//! claim on it: activating the project leaves it where it is, and a verb run
//! for the project with `--project` judges it where it stands. What each of
//! them refuses is stated against the project's own expectations, as
//! alignment states them, and every refusal names `rwv activate <project>
//! --update`, the one spelling that aligns the project's members before it
//! presents the project.

use std::path::{Path, PathBuf};

use crate::alignment::{Accounting, Aligner, Member};
use crate::diagnosis::{judge_edits, Report};
use crate::manifest::{LockFile, Manifest, ProjectName, RepoEntry, RepoPath, Role};
use crate::vcs::{vcs_for, Vcs};
use crate::workspace::{observe_root, project_dir, RootObservation, WorkspaceContext};

/// What a refusal over shared members judges them on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// Edits alone: the check made before anything is aligned.
    Edits,
    /// What an activation refuses: a member holding edits, else one not
    /// aligned for the incoming project.
    Activation,
    /// What a verb that would move members refuses: a member not aligned for
    /// the project, held by edits or not.
    Movement,
}

/// The project the weave root at `root` presents, when it is not `project`.
pub(crate) fn outgoing(root: &Path, project: &ProjectName) -> Option<ProjectName> {
    observe_root(root)
        .as_ref()
        .and_then(RootObservation::presented_project)
        .filter(|presented| *presented != project)
        .cloned()
}

/// Refuse an activation of `project` over a member it shares with the project
/// the root presents that holds edits or is not aligned for `project`.
pub(crate) fn refuse_for_activation(root: &Path, project: &ProjectName) -> anyhow::Result<()> {
    refuse(root, project, Rule::Activation, &|_, _| true)
}

/// Refuse an activation of `project` over a member it shares with the project
/// the root presents that holds edits, before anything is aligned.
pub(crate) fn refuse_edits(root: &Path, project: &ProjectName) -> anyhow::Result<()> {
    refuse(root, project, Rule::Edits, &|_, _| true)
}

/// Refuse a verb run for `project`, which the root does not present, that
/// would move a member `project` shares with the project the root presents:
/// one `covers` names that is not aligned for `project`.
pub(crate) fn refuse_moving(
    root: &Path,
    project: &ProjectName,
    covers: &dyn Fn(&RepoPath, Role) -> bool,
) -> anyhow::Result<()> {
    refuse(root, project, Rule::Movement, covers)
}

struct Held {
    path: RepoPath,
    entry: RepoEntry,
    vcs: Box<dyn Vcs>,
    dir: PathBuf,
}

fn refuse(
    root: &Path,
    project: &ProjectName,
    rule: Rule,
    covers: &dyn Fn(&RepoPath, Role) -> bool,
) -> anyhow::Result<()> {
    let Some(outgoing) = outgoing(root, project) else {
        return Ok(());
    };
    let shared = shared_members(root, project, &outgoing, covers)?;
    if shared.is_empty() {
        return Ok(());
    }
    let mut report = Report::default();
    if rule == Rule::Activation {
        crate::branch::require_recorded(&WorkspaceContext::resolve_for_project(root, project)?)?;
    }
    let aligner = match rule {
        Rule::Edits => None,
        Rule::Activation | Rule::Movement => {
            Some(Aligner::open(root, project, Accounting::scan(root)?)?)
        }
    };
    let lock = lock_of(root, project)?;
    for held in &shared {
        let member = Member {
            path: &held.path,
            entry: &held.entry,
            dir: &held.dir,
            vcs: held.vcs.as_ref(),
        };
        let off = match &aligner {
            Some(aligner) => aligner.activation_line(&member, lock.as_ref(), &held.path)?,
            None => None,
        };
        let edits = || -> anyhow::Result<_> {
            let dirty = held.vcs.tracked_dirty_file_names(&held.dir)?;
            Ok(judge_edits(
                held.vcs.as_ref(),
                &held.path,
                &held.dir,
                dirty,
                None,
            ))
        };
        match rule {
            Rule::Edits => report.extend(edits()?),
            Rule::Activation => match edits()? {
                Some(held_by_edits) => report.add(held_by_edits),
                None => report.extend(off),
            },
            Rule::Movement => {
                if let Some(off) = off {
                    match edits()? {
                        Some(held_by_edits) => report.add(held_by_edits),
                        None => report.add(off),
                    }
                }
            }
        }
    }
    report.into_result()
}

fn shared_members(
    root: &Path,
    project: &ProjectName,
    outgoing: &ProjectName,
    covers: &dyn Fn(&RepoPath, Role) -> bool,
) -> anyhow::Result<Vec<Held>> {
    let ours_path = project_dir(root, project).join(Manifest::FILE_NAME);
    let theirs_path = project_dir(root, outgoing).join(Manifest::FILE_NAME);
    if !ours_path.exists() || !theirs_path.exists() {
        return Ok(Vec::new());
    }
    let ours = Manifest::from_path(&ours_path)?;
    let theirs = Manifest::from_path(&theirs_path)?;
    Ok(ours
        .iter_entries()
        .filter(|(path, entry)| theirs.contains_repo(path) && covers(path, entry.role))
        .map(|(path, entry)| Held {
            path: path.clone(),
            entry: entry.clone(),
            vcs: vcs_for(entry.vcs_type),
            dir: root.join(path.as_path()),
        })
        .filter(|held| {
            held.vcs.is_repo(&held.dir)
                && !crate::workweave::is_symlinked_member(&held.dir)
                && !crate::workweave::is_dangling_link(&held.dir)
        })
        .collect())
}

fn lock_of(root: &Path, project: &ProjectName) -> anyhow::Result<Option<LockFile>> {
    let path = LockFile::path_in(&project_dir(root, project));
    if path.exists() {
        Ok(Some(LockFile::from_state(&path)?))
    } else {
        Ok(None)
    }
}
