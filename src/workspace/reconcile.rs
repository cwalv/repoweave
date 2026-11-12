//! The three records of a workweave root: its marker, the project clone's
//! index, and the worktree relation between the root's project checkout and
//! that clone. Where two of them agree and the third does not, the third is
//! restored from the two.
//!
//! The marker keeps `primary`, `project` and `parent`; the index keeps each
//! workweave's directory and the same `parent`. Together with the relation
//! they restore a lost or edited marker whole, and a root the marker and the
//! relation agree on is listed again when the index lacks it.

use super::{
    discover_projects, is_workspace_root, observe_marker, project_dir, project_rel_dir,
    read_active_pointer, walk_to_weave_root, ActivePointer, MarkerDefect, MarkerPresence,
    WorkweaveMarker,
};
use crate::manifest::{ProjectName, WorkweaveName};
use crate::path_spelling::CanonicalPath;
use crate::refusal::RefusalKind;
use crate::vcs::project_vcs;
use std::path::Path;

/// What a verb does with a disagreement it finds at the root it runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Repair the record that is out of step, and say so.
    Restore,
    /// Repair nothing: a verb that destroys the workweave acts on one
    /// consistent record or none.
    Refuse,
}

/// The primary whose project clone `root`'s checkout of `project` is a linked
/// worktree of, when it is one.
///
/// `None` when the project directory is its own clone, is not a repository,
/// or is not registered by the clone it links to.
pub(crate) fn relation_primary(root: &Path, project: &ProjectName) -> Option<CanonicalPath> {
    let vcs = project_vcs();
    let checkout = project_dir(root, project);
    let clone = CanonicalPath::existing(&vcs.main_checkout(&checkout)?).ok()?;
    if clone.same_directory(&checkout) {
        return None;
    }
    let store = vcs.store_of(&checkout)?;
    vcs.worktree_registration(&store, &checkout)
        .ok()
        .flatten()?;
    let depth = project_rel_dir(project).components().count();
    let primary = clone.as_path().ancestors().nth(depth)?;
    let sits_at = CanonicalPath::existing(&primary.join(project_rel_dir(project))).ok()?;
    (sits_at == clone && is_workspace_root(primary)).then(|| CanonicalPath::of(primary))
}

/// How the marker at a root fails to say what the index and the relation say.
#[derive(Debug)]
pub struct Disagreement {
    root: CanonicalPath,
    project: ProjectName,
    primary: CanonicalPath,
    repair: Repair,
}

#[derive(Debug)]
enum Repair {
    RestoreMarker {
        expected: WorkweaveMarker,
        found: String,
    },
    ListRoot {
        name: WorkweaveName,
        parent: CanonicalPath,
    },
}

impl Disagreement {
    pub fn project(&self) -> &ProjectName {
        &self.project
    }

    pub fn what(&self) -> String {
        match &self.repair {
            Repair::RestoreMarker { found, .. } => format!(
                "the project clone's index lists {} and its project checkout is a worktree of \
                 that clone, but the marker {found}",
                self.root.as_path().display()
            ),
            Repair::ListRoot { name, .. } => format!(
                "the marker and the project checkout agree that {} is workweave `{name}` of \
                 project `{}`, but the project clone's index does not list it",
                self.root.as_path().display(),
                self.project
            ),
        }
    }

    /// Repair the record that is out of step. Returns the line that reports
    /// what was done.
    pub fn apply(&self) -> anyhow::Result<String> {
        match &self.repair {
            Repair::RestoreMarker { expected, .. } => {
                expected.write(self.root.as_path())?;
                Ok(format!(
                    "restored {} from the project clone's index and the worktree relation",
                    WorkweaveMarker::path_in(self.root.as_path())
                ))
            }
            Repair::ListRoot { name, parent } => {
                crate::workweave_index::record_workweave(
                    self.primary.as_path(),
                    &self.project,
                    name,
                    self.root.clone(),
                    parent.clone(),
                )?;
                Ok(format!(
                    "listed workweave `{name}` at {} in the project clone's index",
                    self.root.as_path().display()
                ))
            }
        }
    }

    pub fn refusal(&self) -> anyhow::Error {
        crate::refusal::refusal(
            RefusalKind::StaleRegistryEntry,
            format!(
                "the marker, the index and the worktree relation disagree about {}: {}. A \
                 verb that destroys a workweave acts on records that agree, so it restores \
                 nothing and deletes nothing. Run `rwv doctor --fix`, then retry.",
                self.root.as_path().display(),
                self.what()
            ),
        )
    }
}

fn describe_found(presence: &MarkerPresence) -> String {
    match presence {
        MarkerPresence::Absent => "is missing".to_owned(),
        MarkerPresence::Usable(marker) => format!(
            "names primary {}, project `{}` and parent {}",
            marker.primary().as_path().display(),
            marker.project(),
            marker.parent().as_path().display()
        ),
        MarkerPresence::Defective { .. } => "cannot be read".to_owned(),
    }
}

fn candidate_projects(root: &Path, presence: &MarkerPresence) -> Vec<ProjectName> {
    match presence {
        MarkerPresence::Usable(marker) => vec![marker.project().clone()],
        MarkerPresence::Defective {
            defect: MarkerDefect::Legacy | MarkerDefect::DanglingPrimary { .. },
            ..
        } => Vec::new(),
        MarkerPresence::Absent | MarkerPresence::Defective { .. } => {
            if matches!(read_active_pointer(root), ActivePointer::Present(_)) {
                return Vec::new();
            }
            discover_projects(root)
                .into_iter()
                .filter(|project| relation_primary(root, project).is_some())
                .collect()
        }
    }
}

/// Where the records at `root` disagree, if they do.
///
/// None while the index the other two are judged against is at the old path:
/// it is read by nothing until it is converted, and the verbs that read it
/// refuse then.
pub fn disagreement_at(root: &Path) -> anyhow::Result<Option<Disagreement>> {
    let presence = observe_marker(&WorkweaveMarker::path_in(root));
    for project in candidate_projects(root, &presence) {
        let Some(primary) = relation_primary(root, &project) else {
            continue;
        };
        if crate::workweave_index::legacy_index_path(primary.as_path(), &project).exists() {
            continue;
        }
        let root = CanonicalPath::of(root);
        let index = crate::workweave_index::read(primary.as_path(), &project)?;
        let entry = index
            .as_ref()
            .and_then(|index| index.entry_at(root.as_path()));
        let repair = match (entry, &presence) {
            (Some((_, entry)), _) => {
                let expected = WorkweaveMarker::new(
                    primary.as_path(),
                    project.clone(),
                    entry.parent.as_path(),
                );
                let agrees = matches!(&presence, MarkerPresence::Usable(found)
                    if found.names_primary(&primary)
                        && found.project() == &project
                        && found.parent() == &entry.parent);
                if agrees {
                    continue;
                }
                Repair::RestoreMarker {
                    expected,
                    found: describe_found(&presence),
                }
            }
            (None, MarkerPresence::Usable(marker))
                if marker.names_primary(&primary) && marker.project() == &project =>
            {
                let Some(name) = root
                    .as_path()
                    .file_name()
                    .and_then(|base| base.to_str())
                    .and_then(|base| super::workweave_name_in(&project, base))
                else {
                    continue;
                };
                Repair::ListRoot {
                    name,
                    parent: marker.parent().clone(),
                }
            }
            (None, _) => continue,
        };
        return Ok(Some(Disagreement {
            root,
            project,
            primary,
            repair,
        }));
    }
    Ok(None)
}

/// Reconcile the root `cwd` runs in, before the verb resolves it.
///
/// A directory that is no weave root, or whose records agree, is left alone.
/// `policy` decides from the root what the verb will do there.
pub fn reconcile_invocation(
    cwd: &Path,
    policy: impl FnOnce(&Path) -> anyhow::Result<Policy>,
) -> anyhow::Result<()> {
    let Ok((site, _)) = walk_to_weave_root(cwd) else {
        return Ok(());
    };
    let Some(disagreement) = disagreement_at(site.dir.as_path())? else {
        return Ok(());
    };
    match policy(site.dir.as_path())? {
        Policy::Restore => {
            eprintln!("rwv: {}", disagreement.apply()?);
            Ok(())
        }
        Policy::Refuse => Err(disagreement.refusal()),
    }
}

/// Refuse unless the marker, the index entry at `dir` and the worktree
/// relation agree that `dir` is a workweave of `primary`'s `project`.
pub fn require_agreement(
    primary: &CanonicalPath,
    project: &ProjectName,
    dir: &Path,
) -> anyhow::Result<()> {
    if relation_primary(dir, project).as_ref() != Some(primary) {
        return Err(crate::refusal::refusal(
            RefusalKind::StaleRegistryEntry,
            format!(
                "the marker, the index and the worktree relation disagree about {}: its \
                 project checkout is not a worktree of the project clone this workspace \
                 keeps for `{project}`. A verb that destroys a workweave acts on records \
                 that agree, so it deletes nothing. Run `rwv doctor --fix`, then retry.",
                dir.display()
            ),
        ));
    }
    match disagreement_at(dir)? {
        Some(disagreement) => Err(disagreement.refusal()),
        None => Ok(()),
    }
}

/// Point the recorded parent of the workweave at `dir`, whose marker is
/// `marker`, at `parent`.
///
/// The index is written before the marker: reconcile restores the marker from
/// the index, so a stop between the two writes is carried forward to `parent`
/// by the next verb run there, never back.
pub fn reparent(
    primary: &Path,
    dir: &Path,
    mut marker: WorkweaveMarker,
    parent: &Path,
) -> anyhow::Result<()> {
    crate::workweave_index::set_parent_of(
        primary,
        marker.project(),
        dir,
        CanonicalPath::of(parent),
    )?;
    crate::test_pause::point("reparent");
    marker.repoint_parent(parent);
    marker.write(dir)
}

/// Restore the marker of the workweave `name` the index records at `entry`,
/// when its project checkout is still a worktree of `primary`'s clone.
/// Whether it was restored is returned: `false` leaves the entry for the
/// caller to judge stale.
pub fn restore_marker_of_entry(
    primary: &CanonicalPath,
    project: &ProjectName,
    entry: &crate::workweave_index::IndexedWorkweave,
) -> anyhow::Result<bool> {
    if !entry.path.as_path().is_dir()
        || relation_primary(entry.path.as_path(), project).as_ref() != Some(primary)
    {
        return Ok(false);
    }
    WorkweaveMarker::new(primary.as_path(), project.clone(), entry.parent.as_path())
        .write(entry.path.as_path())?;
    Ok(true)
}
