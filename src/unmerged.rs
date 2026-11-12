//! Merges in a pushed range that joined a member's pins without joining the
//! member: the pushed lock must hold, for each, a commit containing both
//! pins, unless the tip declares the member elsewhere or a later commit in
//! the range records re-pinning it by name to a commit that abandons the pin
//! the merge left behind.

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
    /// A commit in the range re-pinned the member backward by name,
    /// abandoning the pin the merge left unmerged.
    Waived {
        by: CommitId,
        to: CommitId,
        verb: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub(crate) merge: CommitId,
    pub(crate) member: RepoPath,
    /// The pins the merge's parents held.
    pub(crate) pins: (CommitId, CommitId),
    pub(crate) verdict: Verdict,
    /// Commits at or after the merge whose record of re-pinning the member
    /// disagrees with what their lock changed: they waive nothing.
    pub(crate) untrusted: Vec<CommitId>,
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
                let (verdict, untrusted) = if !same_branch {
                    (Verdict::ListedOnly, Vec::new())
                } else {
                    let found = waiver(&mut reader, member, range, index, &pins)?;
                    (
                        found.waived.unwrap_or(Verdict::Unmerged),
                        found.untrusted,
                    )
                };
                findings.push(Finding {
                    merge: merge.id.clone(),
                    member: member.path.clone(),
                    pins,
                    verdict,
                    untrusted,
                });
            }
        }
    }
    Ok(findings)
}

struct Search {
    waived: Option<Verdict>,
    untrusted: Vec<CommitId>,
}

/// The first commit at or after `range[index]`, the merge's own included,
/// that records re-pinning the member backward by name to a commit reaching
/// no pin the merge left unmerged. A record whose pins are not the ones the
/// commit's first parent and the commit itself hold is untrusted.
fn waiver(
    reader: &mut Reader<'_>,
    member: &Member<'_>,
    range: &[RangeCommit],
    index: usize,
    pins: &(CommitId, CommitId),
) -> anyhow::Result<Search> {
    let merge = &range[index];
    let left = reader.held(&merge.id).pins.get(member.path).cloned();
    let unmerged: Vec<&CommitId> = [&pins.0, &pins.1]
        .into_iter()
        .filter(|pin| match &left {
            Some(left) => reaches(member, pin, left) != Some(true),
            None => true,
        })
        .collect();
    let mut search = Search {
        waived: None,
        untrusted: Vec::new(),
    };
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
        let records: Vec<crate::repin::Repin> =
            crate::repin::recorded(reader.vcs, reader.dir, &candidate.id)?
                .into_iter()
                .filter(|repin| &repin.member == member.path)
                .collect();
        if records.is_empty() {
            continue;
        }
        let to = reader.held(&candidate.id).pins.get(member.path).cloned();
        let from = candidate
            .parents
            .first()
            .and_then(|parent| reader.held(parent).pins.get(member.path).cloned());
        for repin in records {
            if Some(&repin.to) != to.as_ref() || repin.from != from {
                search.untrusted.push(candidate.id.clone());
                continue;
            }
            let backward = repin
                .from
                .as_ref()
                .is_some_and(|from| reaches(member, from, &repin.to) == Some(false));
            let abandons = unmerged
                .iter()
                .all(|pin| reaches(member, pin, &repin.to) == Some(false));
            if backward && abandons && search.waived.is_none() {
                search.waived = Some(Verdict::Waived {
                    by: candidate.id.clone(),
                    to: repin.to,
                    verb: repin.verb,
                });
            }
        }
    }
    Ok(search)
}
