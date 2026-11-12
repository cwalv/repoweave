//! Merges in a pushed range that joined a member's pins without joining the
//! member: the pushed lock must hold, for each, a commit containing both
//! pins, unless the tip declares the member elsewhere or a later lock in the
//! range abandons the pin the merge left behind.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::lock_merge::Held;
use crate::manifest::{LockFile, Manifest, RepoPath};
use crate::vcs::{CommitId, RangeCommit, ResolvedRevisionId, Vcs};

/// A member checkout the analysis asks about ancestry in.
pub(crate) struct Member<'a> {
    pub(crate) path: &'a RepoPath,
    pub(crate) dir: PathBuf,
    pub(crate) vcs: &'a dyn Vcs,
}

/// How a merge stands for one member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The pushed lock holds neither a commit containing both pins nor a
    /// waiver.
    Unmerged,
    /// The parents declare the member differently, so no one commit could
    /// contain both pins: listed, not refused.
    ListedOnly,
    /// A lock in the range abandoned the pin the merge left unmerged.
    Waived { by: CommitId, to: CommitId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub(crate) merge: CommitId,
    pub(crate) member: RepoPath,
    /// The pins the merge's parents held.
    pub(crate) pins: (CommitId, CommitId),
    pub(crate) verdict: Verdict,
}

struct Reader<'a> {
    vcs: &'a dyn Vcs,
    dir: &'a Path,
    cache: HashMap<CommitId, Held>,
}

impl Reader<'_> {
    fn held(&mut self, commit: &CommitId) -> &Held {
        if !self.cache.contains_key(commit) {
            let held = held_at(self.vcs, self.dir, commit);
            self.cache.insert(commit.clone(), held);
        }
        &self.cache[commit]
    }
}

/// The pins and declared branches `commit` of the project repo at `dir`
/// holds; a file that is absent or does not parse holds nothing.
pub(crate) fn held_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Held {
    let at = ResolvedRevisionId::from(commit.clone());
    let mut held = Held::default();
    if let Some(lock) = vcs
        .read_file_at_revision(dir, &at, Path::new(LockFile::FILE_NAME))
        .ok()
        .and_then(|text| LockFile::from_json_str(&text).ok())
    {
        for (path, entry) in lock.iter_entries() {
            if let Some(pin) = entry.version.as_commit_id() {
                held.pins.insert(path.clone(), pin);
            }
        }
    }
    if let Some(manifest) = vcs
        .read_file_at_revision(dir, &at, Path::new(Manifest::FILE_NAME))
        .ok()
        .and_then(|text| Manifest::from_toml_str(&text).ok())
    {
        for (path, entry) in manifest.iter_entries() {
            held.declared.insert(
                path.clone(),
                entry.branch.local_counterpart().as_str().to_owned(),
            );
        }
    }
    held
}

fn reaches(member: &Member<'_>, ancestor: &CommitId, of: &CommitId) -> Option<bool> {
    member
        .vcs
        .is_ancestor(
            &member.dir,
            &ResolvedRevisionId::from(ancestor.clone()),
            &ResolvedRevisionId::from(of.clone()),
        )
        .ok()
}

/// Every member each merge of `range` left unjoined, judged against `tip`'s
/// lock. `range` is oldest first; `project_dir` is the project repo whose
/// history it is.
pub(crate) fn analyze(
    project_vcs: &dyn Vcs,
    project_dir: &Path,
    members: &[Member<'_>],
    range: &[RangeCommit],
    tip: &CommitId,
) -> anyhow::Result<Vec<Finding>> {
    let mut reader = Reader {
        vcs: project_vcs,
        dir: project_dir,
        cache: HashMap::new(),
    };
    let tip_held = reader.held(tip).clone();
    let mut findings = Vec::new();
    for (index, merge) in range.iter().enumerate() {
        let Some((first, others)) = merge.parents.split_first() else {
            continue;
        };
        for other in others {
            let base = project_vcs
                .merge_bases(project_dir, first, other)?
                .into_iter()
                .next();
            let base_held = base.as_ref().map(|base| reader.held(base).clone());
            let first_held = reader.held(first).clone();
            let other_held = reader.held(other).clone();
            for member in members {
                let (Some(first_pin), Some(other_pin)) = (
                    first_held.pins.get(member.path),
                    other_held.pins.get(member.path),
                ) else {
                    continue;
                };
                let base_pin = base_held
                    .as_ref()
                    .and_then(|held| held.pins.get(member.path));
                if first_pin == other_pin
                    || Some(first_pin) == base_pin
                    || Some(other_pin) == base_pin
                {
                    continue;
                }
                let declared = first_held.declared.get(member.path);
                let same_branch =
                    declared.is_some() && declared == other_held.declared.get(member.path);
                if same_branch && tip_held.declared.get(member.path) != declared {
                    continue;
                }
                let pins = (first_pin.clone(), other_pin.clone());
                if let Some(tip_pin) = tip_held.pins.get(member.path) {
                    let has_first = reaches(member, first_pin, tip_pin) == Some(true);
                    let has_other = reaches(member, other_pin, tip_pin) == Some(true);
                    if has_first && has_other {
                        continue;
                    }
                }
                let verdict = if !same_branch {
                    Verdict::ListedOnly
                } else {
                    match waiver(&mut reader, member, range, index, &pins) {
                        Some((by, to)) => Verdict::Waived { by, to },
                        None => Verdict::Unmerged,
                    }
                };
                findings.push(Finding {
                    merge: merge.id.clone(),
                    member: member.path.clone(),
                    pins,
                    verdict,
                });
            }
        }
    }
    Ok(findings)
}

/// The first lock at or after `range[index]` that re-pins the member
/// backward, to a commit that reaches neither pin the merge left behind. The
/// merge's own lock counts only when it pins neither parent's commit: keeping
/// one side's pin is how a conflict is settled by hand, which names nothing.
fn waiver(
    reader: &mut Reader<'_>,
    member: &Member<'_>,
    range: &[RangeCommit],
    index: usize,
    pins: &(CommitId, CommitId),
) -> Option<(CommitId, CommitId)> {
    let merge = &range[index];
    let left = reader.held(&merge.id).pins.get(member.path).cloned();
    let unmerged: Vec<&CommitId> = [&pins.0, &pins.1]
        .into_iter()
        .filter(|pin| match &left {
            Some(left) => reaches(member, pin, left) != Some(true),
            None => true,
        })
        .collect();
    let mut descended: BTreeSet<&CommitId> = BTreeSet::new();
    descended.insert(&merge.id);
    for candidate in &range[index..] {
        let follows = candidate.id == merge.id
            || candidate
                .parents
                .iter()
                .any(|parent| descended.contains(parent));
        if !follows {
            continue;
        }
        descended.insert(&candidate.id);
        let Some(to) = reader.held(&candidate.id).pins.get(member.path).cloned() else {
            continue;
        };
        if candidate.id == merge.id && (to == pins.0 || to == pins.1) {
            continue;
        }
        let Some(from) = candidate
            .parents
            .first()
            .and_then(|parent| reader.held(parent).pins.get(member.path).cloned())
        else {
            continue;
        };
        let backward = to != from && reaches(member, &from, &to) == Some(false);
        let abandons = unmerged
            .iter()
            .all(|pin| reaches(member, pin, &to) == Some(false));
        if backward && abandons {
            return Some((candidate.id.clone(), to));
        }
    }
    None
}
