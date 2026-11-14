//! The pins of a rebased project branch, mapped through the member rebases:
//! each replayed project commit pins what the commits it pinned became.

use crate::manifest::{LockEntry, LockFile, Manifest, RepoKey, RepoPath};
use crate::op_state::Rebasing;
use crate::vcs::{vcs_for, CommitId, RawRefName, RawRevisionId, Replayed, ResolvedRevisionId, Vcs};
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
    /// The commit the replay made of `pin`, or for a commit it dropped, the
    /// one it made of the nearest commit before it; `None` where the replay
    /// did not hold `pin`.
    pub(crate) fn image_of(&self, pin: &CommitId) -> Option<CommitId> {
        match self.image(pin)? {
            Image::Replayed(now) | Image::Dropped(now) => Some(now),
        }
    }

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

/// What the replay of `from` onto `onto` recorded of each commit of
/// `base..from`, read against `tip`; refused where no record describes
/// `tip`, since which commit became which is then unknown.
#[allow(clippy::too_many_arguments)]
pub(crate) fn recorded(
    vcs: &dyn Vcs,
    dir: &Path,
    label: &str,
    base: &CommitId,
    from: &CommitId,
    onto: &CommitId,
    tip: &CommitId,
    held_by: HeldBy<'_>,
) -> anyhow::Result<Replayed> {
    let Some(replayed) = vcs.replayed(dir, base, from, onto, tip)? else {
        return Err(crate::refusal::refusal(
            crate::refusal::RefusalKind::ReplayUnrecorded,
            format!(
                "{label}: no record of the rebase of {} onto {} describes the tip {}, so \
                 which commit became which is unknown and no pin is moved by a guess; {}",
                from.short(),
                onto.short(),
                tip.short(),
                held_by.abort_choice(tip)
            ),
        ));
    };
    Ok(replayed)
}

/// The repo whose tip a refusal names, in the workspace holding the record
/// of the operation that moved it.
#[derive(Clone, Copy)]
pub(crate) struct HeldBy<'a> {
    pub(crate) workspace: &'a Path,
    pub(crate) repo: &'a RepoKey,
}

impl HeldBy<'_> {
    fn abort_choice(&self, tip: &CommitId) -> String {
        let Ok(Some(owner)) = crate::op_state::read_owner(self.workspace) else {
            return "this puts back what this operation moved:\n\trwv abort".to_owned();
        };
        let written = match &owner.tips {
            crate::op_state::PhaseTips::Replay(tips)
            | crate::op_state::PhaseTips::Converged(tips) => tips.get(self.repo),
        };
        if written.is_none_or(|written| written == tip) {
            return "this puts back what this operation moved:\n\trwv abort".to_owned();
        }
        let operand = match self.repo {
            RepoKey::Project => crate::manifest::RepoOperand::Project(owner.project.clone()),
            RepoKey::Member(path) => crate::manifest::RepoOperand::Member(path.clone()),
        };
        format!(
            "the tip is not one this operation wrote, so `rwv abort` refuses it as foreign; \
             this puts the branch back and keeps that tip as an `abandoned` backup:\n\t\
             rwv abort --abandon-foreign-tip {operand}"
        )
    }
}

/// The member rebases of `rebases`, each read back from its branch's tip in
/// its checkout under `root`, one whose branch is gone or still holds the
/// commits the rebase started from (integrated by a merge instead) left out,
/// and the
/// member carries of `carries` that composed, each read back from the tip
/// it composed.
pub(crate) fn member_replays(
    root: &Path,
    manifest: &Manifest,
    rebases: &[Rebasing],
    carries: &[crate::op_state::Carrying],
    workspace: &Path,
) -> anyhow::Result<BTreeMap<RepoPath, Vec<MemberReplay>>> {
    let mut replays: BTreeMap<RepoPath, Vec<MemberReplay>> = BTreeMap::new();
    for carry in carries {
        let (RepoKey::Member(path), Some(composed)) = (&carry.repo, &carry.composed) else {
            continue;
        };
        let Some(entry) = manifest.get_entry(path) else {
            continue;
        };
        let replayed = recorded(
            vcs_for(entry.vcs_type).as_ref(),
            &root.join(path.as_path()),
            path.as_str(),
            &carry.base,
            &carry.from,
            &carry.onto,
            composed,
            HeldBy {
                workspace,
                repo: &carry.repo,
            },
        )?;
        replays.entry(path.clone()).or_default().push(MemberReplay {
            replayed,
            onto: carry.onto.clone(),
        });
    }
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
        let from = crate::vcs::ResolvedRevisionId::from(rebase.from.clone());
        if vcs.is_ancestor(&dir, &from, &tip)? {
            continue;
        }
        let replayed = recorded(
            vcs.as_ref(),
            &dir,
            path.as_str(),
            &rebase.base,
            &rebase.from,
            &rebase.onto,
            tip.id(),
            HeldBy {
                workspace,
                repo: &rebase.repo,
            },
        )?;
        replays.entry(path.clone()).or_default().push(MemberReplay {
            replayed,
            onto: rebase.onto.clone(),
        });
    }
    Ok(replays)
}

/// The verb a re-pin records where a rebase of the member moved its pin.
pub(crate) const REBASE_VERB: &str = "rebase";

/// Each member of `rebases` whose pin in `from` its replay maps to its pin in
/// `to`, or to a commit on that pin's first-parent chain: the rebase's re-pin
/// of it by name, from the one pin to the other. A member the replay did not
/// hold, or whose pins it does not map so, is left out.
pub(crate) fn rebase_jumps(
    root: &Path,
    manifest: &Manifest,
    rebases: &[Rebasing],
    from: &BTreeMap<RepoPath, CommitId>,
    to: &BTreeMap<RepoPath, CommitId>,
    workspace: &Path,
) -> anyhow::Result<Vec<crate::repin::Repin>> {
    let rebases: Vec<Rebasing> = rebases
        .iter()
        .filter(|rebase| matches!(rebase.repo, RepoKey::Member(_)))
        .cloned()
        .collect();
    if rebases.is_empty() {
        return Ok(Vec::new());
    }
    let mut jumps = Vec::new();
    for (member, replays) in &member_replays(root, manifest, &rebases, &[], workspace)? {
        let (Some(was), Some(now)) = (from.get(member), to.get(member)) else {
            continue;
        };
        let Some(entry) = manifest.get_entry(member) else {
            continue;
        };
        let checkout = root.join(member.as_path());
        let vcs = vcs_for(entry.vcs_type);
        jumps.extend(jump(member, replays, was, now, |image| {
            Ok(vcs.on_first_parents(&checkout, now, image)?)
        })?);
    }
    Ok(jumps)
}

/// The re-pin of `member` from `was` to `now` where `replays` maps `was` to
/// `now`, or to a commit `on_chain` finds on `now`'s first-parent chain.
fn jump(
    member: &RepoPath,
    replays: &[MemberReplay],
    was: &CommitId,
    now: &CommitId,
    on_chain: impl FnOnce(&CommitId) -> anyhow::Result<bool>,
) -> anyhow::Result<Option<crate::repin::Repin>> {
    if was == now {
        return Ok(None);
    }
    let Some(image) = replays.iter().find_map(|replay| replay.image_of(was)) else {
        return Ok(None);
    };
    if image != *now && !on_chain(&image)? {
        return Ok(None);
    }
    Ok(Some(crate::repin::Repin {
        member: member.clone(),
        from: Some(was.clone()),
        to: now.clone(),
        verb: REBASE_VERB.to_owned(),
    }))
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

/// Whether `commit` changes nothing but its lock, once its `rwv.toml` is
/// `manifest` where that is given.
fn changes_nothing_but_the_lock(
    vcs: &dyn Vcs,
    dir: &Path,
    commit: &CommitId,
    manifest: Option<&str>,
) -> anyhow::Result<bool> {
    let Some(parent) = vcs.parents(dir, commit)?.into_iter().next() else {
        return Ok(false);
    };
    let parent = ResolvedRevisionId::from(parent);
    if let Some(manifest) = manifest {
        let at_parent = vcs
            .read_file_at_revision(dir, &parent, Path::new(Manifest::FILE_NAME))
            .ok();
        if at_parent.as_deref() != Some(manifest) {
            return Ok(false);
        }
    }
    Ok(vcs
        .changed_paths_between(dir, &parent, &ResolvedRevisionId::from(commit.clone()))?
        .iter()
        .all(|path| {
            path == LockFile::FILE_NAME || (manifest.is_some() && path == Manifest::FILE_NAME)
        }))
}

/// A project branch's re-map: each pin a dropped commit's re-map moved,
/// where any commit changed, the rewritten tip with what the replay's
/// commits became once rewritten, and the replayed tip where the `rwv.toml`
/// given for it left it changing nothing, so it was dropped.
#[derive(Debug, Default)]
pub(crate) struct ProjectRemap {
    pub(crate) remapped: Vec<Remapped>,
    pub(crate) rewritten: Option<(CommitId, Replayed)>,
    pub(crate) emptied: Option<CommitId>,
}

/// Rewrite each commit the project rebase `replayed` wrote onto `onto` so its
/// lock pins what the member rebases made of the commits it pinned, and the
/// last one, where `tip_manifest` is given, so its `rwv.toml` is that.
/// Commits already pinning so are kept as they are; one left changing
/// nothing, as the rebase drops a commit it leaves empty, is dropped. A
/// written commit no replayed commit became, such as a relock written after
/// the replay, keeps the pins it changed, so a re-map of a branch already
/// re-mapped keeps it.
pub(crate) fn remap_project(
    vcs: &dyn Vcs,
    dir: &Path,
    replayed: &Replayed,
    onto: &CommitId,
    replays: &BTreeMap<RepoPath, Vec<MemberReplay>>,
    tip_manifest: Option<&str>,
) -> anyhow::Result<ProjectRemap> {
    let mut became: Vec<(CommitId, CommitId)> = Vec::new();
    let mut remapped = Vec::new();
    let mut parent = onto.clone();
    let mut before = lock_at(vcs, dir, onto);
    let mut rewritten = false;
    let mut emptied = None;
    for written in &replayed.written {
        let carried = tip_manifest.filter(|_| replayed.written.last() == Some(written));
        let manifest = match carried {
            Some(text) => Manifest::from_toml_str(text).ok(),
            None => manifest_at(vcs, dir, written),
        };
        let members: Vec<RepoPath> = match manifest {
            Some(manifest) => manifest.iter_entries().map(|(p, _)| p.clone()).collect(),
            None => lock_at(vcs, dir, written)
                .map(|lock| lock.iter_entries().map(|(p, _)| p.clone()).collect())
                .unwrap_or_default(),
        };
        let original = replayed.original(written).or(Some(written));
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
        let pins_as_before = before
            .as_ref()
            .map(crate::lock::render)
            .transpose()?
            .is_some_and(|text| text == rendered);
        if pins_as_before && changes_nothing_but_the_lock(vcs, dir, written, carried)? {
            if carried.is_some() {
                emptied = Some(written.clone());
            }
            rewritten = true;
            continue;
        }
        let keep = carried.is_none() && !rewritten && current.as_deref() == Some(rendered.as_str());
        parent = if keep {
            written.clone()
        } else {
            rewritten = true;
            crate::repin::copy_with_lock(
                vcs,
                dir,
                written,
                &parent,
                rendered.into_bytes(),
                carried.map(|text| text.as_bytes().to_vec()),
                &mut |member, pin| {
                    map_pin(
                        member,
                        &RawRevisionId::new(pin.as_str()),
                        replays,
                        &mut Vec::new(),
                    )
                    .as_commit_id()
                    .unwrap_or_else(|| pin.clone())
                },
            )?
        };
        became.push((written.clone(), parent.clone()));
        before = Some(lock);
    }
    let now = |image: &CommitId| {
        became
            .iter()
            .find(|(written, _)| written == image)
            .map(|(_, now)| now.clone())
    };
    Ok(ProjectRemap {
        remapped,
        rewritten: rewritten.then(|| {
            (
                parent,
                Replayed {
                    images: replayed
                        .images
                        .iter()
                        .map(|(old, image)| (old.clone(), image.as_ref().and_then(now)))
                        .collect(),
                    written: became.iter().map(|(_, now)| now.clone()).collect(),
                },
            )
        }),
        emptied,
    })
}

/// What re-mapping the project branch did: each pin a dropped commit's
/// re-map moved, and, where the replayed tip was dropped as changing nothing
/// once its `rwv.toml` was the one given, the line that says so.
#[derive(Debug, Default)]
pub(crate) struct RemappedProject {
    pub(crate) remapped: Vec<Remapped>,
    pub(crate) emptied: Option<String>,
}

/// Re-map the project branch checked out in `project_dir` through the
/// rebases journaled in `rebases`, where the project branch is among them,
/// writing `tip_manifest`, where given, as the `rwv.toml` of its copy of the
/// replayed tip; `None` where it was not rebased.
pub(crate) fn remap_rebased_project(
    project_vcs: &dyn Vcs,
    project_dir: &Path,
    root: &Path,
    rebases: &[Rebasing],
    workspace: &Path,
    tip_manifest: Option<&str>,
) -> anyhow::Result<Option<RemappedProject>> {
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
    let replayed = recorded(
        project_vcs,
        project_dir,
        &project_dir.display().to_string(),
        &project.base,
        &project.from,
        &project.onto,
        &tip,
        HeldBy {
            workspace,
            repo: &RepoKey::Project,
        },
    )?;
    let manifest = match tip_manifest {
        Some(text) => Manifest::from_toml_str(text)?,
        None => Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?,
    };
    let replays = member_replays(root, &manifest, rebases, &[], workspace)?;
    let remap = remap_project(
        project_vcs,
        project_dir,
        &replayed,
        &project.onto,
        &replays,
        tip_manifest,
    )?;
    if let Some((rewritten, became)) = &remap.rewritten {
        project_vcs.record_replayed(project_dir, &project.from, &project.onto, became)?;
        crate::test_pause::point("remap-recorded");
        project_vcs.take_rewritten(&on, &tip, rewritten)?;
        crate::test_pause::point("remap-moved");
    }
    Ok(Some(RemappedProject {
        remapped: remap.remapped,
        emptied: remap.emptied.map(|tip| {
            format!(
                "the replayed tip {} is dropped: with the resolved rwv.toml it changes nothing",
                tip.short()
            )
        }),
    }))
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
    fn a_rebase_jump_is_recorded_only_to_the_image_or_a_commit_built_on_it() {
        let member = RepoPath::new("m").unwrap();
        let replays = [replay()];
        let jump_to = |now: u8, on_chain: bool| {
            jump(&member, &replays, &commit(1), &commit(now), |_| {
                Ok(on_chain)
            })
            .unwrap()
            .map(|repin| repin.to)
        };
        assert_eq!(jump_to(11, false), Some(commit(11)));
        assert_eq!(jump_to(12, true), Some(commit(12)));
        assert_eq!(jump_to(12, false), None);
        assert_eq!(
            jump(&member, &replays, &commit(4), &commit(11), |_| Ok(true)).unwrap(),
            None
        );
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

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn lock_pinning(version: &str) -> String {
        format!(
            r#"{{"repositories":{{"github/o/lib":{{"type":"git","url":"https://example.invalid/o/lib.git","version":"{version}"}}}}}}"#
        )
    }

    fn commit_lock(dir: &Path, version: &str) -> CommitId {
        let lock = LockFile::from_json_str(&lock_pinning(version)).unwrap();
        std::fs::write(
            dir.join(LockFile::FILE_NAME),
            crate::lock::render(&lock).unwrap(),
        )
        .unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "lock"]);
        crate::git::git_vcs()
            .head_revision(dir)
            .unwrap()
            .id()
            .clone()
    }

    #[test]
    fn a_written_commit_no_replayed_commit_became_keeps_the_pins_it_changed() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        let onto = commit_lock(dir, &"a".repeat(40));
        let relock = commit_lock(dir, &"b".repeat(40));
        let replayed = Replayed {
            images: Vec::new(),
            written: vec![relock],
        };

        let remap = remap_project(
            crate::git::git_vcs().as_ref(),
            dir,
            &replayed,
            &onto,
            &BTreeMap::new(),
            None,
        )
        .unwrap();

        assert!(remap.rewritten.is_none());
        assert!(remap.remapped.is_empty());
    }
}
