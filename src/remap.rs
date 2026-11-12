//! The pins of a rebased project branch, mapped through the member rebases:
//! each replayed project commit pins what the commits it pinned became.

use crate::manifest::{LockEntry, LockFile, Manifest, RepoKey, RepoPath};
use crate::op_state::Rebasing;
use crate::vcs::{
    vcs_for, AttachedRef, CommitId, RawRefName, RawRevisionId, Replayed, ResolvedRevisionId, Vcs,
};
use std::collections::BTreeMap;
use std::path::Path;

/// One member's rebase, read back from its commits.
#[derive(Debug, Clone)]
pub(crate) struct MemberReplay {
    pub(crate) replayed: Replayed,
    pub(crate) onto: CommitId,
}

/// Where a pin goes when the commit it names was rebased.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Image {
    Replayed(CommitId),
    /// The rebase dropped the pinned commit: the image of the nearest
    /// replayed commit before it, else the commit the rebase replayed onto.
    Dropped(CommitId),
}

impl MemberReplay {
    fn image(&self, pin: &CommitId) -> Option<Image> {
        let at = self
            .replayed
            .images
            .iter()
            .position(|(old, _)| old == pin)?;
        if let Some(new) = &self.replayed.images[at].1 {
            return Some(Image::Replayed(new.clone()));
        }
        let before = self.replayed.images[..at]
            .iter()
            .rev()
            .find_map(|(_, new)| new.clone())
            .unwrap_or_else(|| self.onto.clone());
        Some(Image::Dropped(before))
    }
}

/// The member rebases of `rebases`, each read back from its branch's tip in
/// its checkout under `root`; one whose branch is gone is left out.
pub(crate) fn member_replays(
    root: &Path,
    manifest: &Manifest,
    rebases: &[Rebasing],
) -> anyhow::Result<BTreeMap<RepoPath, Vec<MemberReplay>>> {
    let mut replays: BTreeMap<RepoPath, Vec<MemberReplay>> = BTreeMap::new();
    for rebase in rebases {
        let RepoKey::Member(path) = &rebase.repo else {
            continue;
        };
        let Some(entry) = manifest.get_entry(path) else {
            continue;
        };
        let vcs = vcs_for(entry.vcs_type);
        let dir = root.join(path.as_path());
        let Some(tip) =
            vcs.resolve_local_branch_tip(&dir, &RawRefName::new(rebase.branch.clone()))?
        else {
            continue;
        };
        let replayed = vcs.replayed(&dir, &rebase.onto, &rebase.from, &rebase.onto, tip.id())?;
        replays.entry(path.clone()).or_default().push(MemberReplay {
            replayed,
            onto: rebase.onto.clone(),
        });
    }
    Ok(replays)
}

/// A pin a dropped commit's re-map moved, by the project commit that pinned
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Remapped {
    pub(crate) member: RepoPath,
    pub(crate) was: CommitId,
    pub(crate) now: CommitId,
}

fn map_pin(
    member: &RepoPath,
    pin: &RawRevisionId,
    replays: &BTreeMap<RepoPath, Vec<MemberReplay>>,
    remapped: &mut Vec<Remapped>,
) -> RawRevisionId {
    let Some(commit) = pin.as_commit_id() else {
        return pin.clone();
    };
    let found = replays
        .get(member)
        .into_iter()
        .flatten()
        .find_map(|replay| replay.image(&commit));
    match found {
        None => pin.clone(),
        Some(Image::Replayed(now)) => RawRevisionId::new(now.as_str()),
        Some(Image::Dropped(now)) => {
            let entry = Remapped {
                member: member.clone(),
                was: commit,
                now: now.clone(),
            };
            if !remapped.contains(&entry) {
                remapped.push(entry);
            }
            RawRevisionId::new(now.as_str())
        }
    }
}

fn lock_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<LockFile> {
    let text = vcs
        .read_file_at_revision(
            dir,
            &ResolvedRevisionId::from(commit.clone()),
            Path::new(LockFile::FILE_NAME),
        )
        .ok()?;
    LockFile::from_json_str(&text).ok()
}

fn manifest_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<Manifest> {
    let text = vcs
        .read_file_at_revision(
            dir,
            &ResolvedRevisionId::from(commit.clone()),
            Path::new(Manifest::FILE_NAME),
        )
        .ok()?;
    Manifest::from_toml_str(&text).ok()
}

/// The lock a replayed commit carries: each pin the original commit changed
/// from its parent's, mapped through the member rebases; every other pin as
/// the commit before it in the replay holds it.
fn replayed_lock(
    members: &[RepoPath],
    before: Option<&LockFile>,
    original: Option<&LockFile>,
    original_parent: Option<&LockFile>,
    replays: &BTreeMap<RepoPath, Vec<MemberReplay>>,
    remapped: &mut Vec<Remapped>,
) -> LockFile {
    let mut repositories = BTreeMap::new();
    for member in members {
        let ours = original.and_then(|lock| lock.get_entry(member));
        let parents = original_parent.and_then(|lock| lock.get_entry(member));
        let changed = ours.is_some_and(|o| parents.is_none_or(|p| p.version != o.version));
        let held = before.and_then(|lock| lock.get_entry(member));
        let entry: Option<LockEntry> = match (changed, held, ours) {
            (true, _, Some(ours)) | (false, None, Some(ours)) => Some(LockEntry {
                version: map_pin(member, &ours.version, replays, remapped),
                ..ours.clone()
            }),
            (_, Some(held), _) => Some(held.clone()),
            _ => None,
        };
        if let Some(entry) = entry {
            repositories.insert(member.clone(), entry);
        }
    }
    LockFile { repositories }
}

/// Rewrite each commit the project rebase `replayed` wrote onto `onto` so its
/// lock pins what the member rebases made of the commits it pinned, then move
/// the branch `on` witnesses to the rewritten tip. Commits already pinning so
/// are kept as they are. Returns each pin a dropped commit's re-map moved.
pub(crate) fn remap_project(
    vcs: &dyn Vcs,
    on: &AttachedRef,
    replayed: &Replayed,
    onto: &CommitId,
    replays: &BTreeMap<RepoPath, Vec<MemberReplay>>,
) -> anyhow::Result<Vec<Remapped>> {
    let dir = on.repo();
    let mut remapped = Vec::new();
    let mut parent = onto.clone();
    let mut before = lock_at(vcs, dir, onto);
    let mut rewritten = false;
    for written in &replayed.written {
        let members: Vec<RepoPath> = match manifest_at(vcs, dir, written) {
            Some(manifest) => manifest.iter_entries().map(|(p, _)| p.clone()).collect(),
            None => lock_at(vcs, dir, written)
                .map(|lock| lock.iter_entries().map(|(p, _)| p.clone()).collect())
                .unwrap_or_default(),
        };
        let original = replayed.original(written);
        let original_lock = original.and_then(|o| lock_at(vcs, dir, o));
        let original_parent_lock = match original {
            Some(o) => vcs
                .parents(dir, o)?
                .first()
                .and_then(|p| lock_at(vcs, dir, p)),
            None => None,
        };
        let lock = replayed_lock(
            &members,
            before.as_ref(),
            original_lock.as_ref(),
            original_parent_lock.as_ref(),
            replays,
            &mut remapped,
        );
        let rendered = crate::lock::render(&lock)?;
        let current = vcs
            .read_file_at_revision(
                dir,
                &ResolvedRevisionId::from(written.clone()),
                Path::new(LockFile::FILE_NAME),
            )
            .ok();
        let keep = !rewritten && current.as_deref() == Some(rendered.as_str());
        parent = if keep {
            written.clone()
        } else {
            rewritten = true;
            crate::repin::copy_with_lock(vcs, dir, written, &parent, rendered.into_bytes())?
        };
        before = Some(lock);
    }
    if rewritten {
        let last = replayed
            .written
            .last()
            .expect("a rewrite rewrote a written commit");
        vcs.take_rewritten(on, last, &parent)?;
    }
    Ok(remapped)
}

/// Re-map the project branch checked out in `project_dir` through the
/// rebases journaled in `rebases`, where the project branch is among them;
/// `None` where it was not rebased.
pub(crate) fn remap_rebased_project(
    project_vcs: &dyn Vcs,
    project_dir: &Path,
    root: &Path,
    rebases: &[Rebasing],
) -> anyhow::Result<Option<Vec<Remapped>>> {
    let Some(project) = rebases.iter().find(|r| r.repo == RepoKey::Project) else {
        return Ok(None);
    };
    let crate::vcs::HeadAttachment::Attached(on) = project_vcs.head_attachment(project_dir)? else {
        return Err(crate::untokened!(
            EnvironmentPassthrough,
            "{}: left its branch '{}' after its rebase",
            project_dir.display(),
            project.branch
        ));
    };
    let tip = project_vcs.head_revision(project_dir)?.id().clone();
    let replayed = project_vcs.replayed(
        project_dir,
        &project.onto,
        &project.from,
        &project.onto,
        &tip,
    )?;
    let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let replays = member_replays(root, &manifest, rebases)?;
    remap_project(project_vcs, &on, &replayed, &project.onto, &replays).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(n: u8) -> CommitId {
        CommitId::parse(&format!("{n:040x}")).unwrap()
    }

    fn replay() -> MemberReplay {
        MemberReplay {
            replayed: Replayed {
                images: vec![
                    (commit(1), Some(commit(11))),
                    (commit(2), None),
                    (commit(3), Some(commit(13))),
                ],
                written: vec![commit(11), commit(13)],
            },
            onto: commit(9),
        }
    }

    #[test]
    fn a_pin_on_a_dropped_commit_takes_the_image_of_the_replayed_commit_before_it() {
        assert_eq!(
            replay().image(&commit(1)),
            Some(Image::Replayed(commit(11)))
        );
        assert_eq!(replay().image(&commit(2)), Some(Image::Dropped(commit(11))));
        assert_eq!(replay().image(&commit(4)), None);
    }

    #[test]
    fn a_dropped_first_commit_maps_to_the_commit_replayed_onto() {
        let mut first_dropped = replay();
        first_dropped.replayed.images[0].1 = None;
        assert_eq!(
            first_dropped.image(&commit(1)),
            Some(Image::Dropped(commit(9)))
        );
    }
}
