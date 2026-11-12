//! The record a commit carries of each member a verb re-pinned by name: one
//! `Rwv-Repin` annotation per member whose pin the commit moves, naming the
//! member, its old pin, its new pin and the verb.

use crate::manifest::{LockFile, RepoPath};
use crate::vcs::{Annotation, CommitId, ResolvedRevisionId, Vcs, VcsError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(crate) const KEY: &str = "Rwv-Repin";

const NO_PIN: &str = "none";

/// Whether a lock commit re-pins members by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Naming {
    /// No member is named: a relock, a merge, a first pin.
    Unnamed,
    /// `verb` names each of `members`.
    ByName {
        verb: &'static str,
        members: BTreeSet<RepoPath>,
    },
}

impl Naming {
    pub(crate) fn by_name(verb: &'static str, members: impl IntoIterator<Item = RepoPath>) -> Self {
        let members: BTreeSet<RepoPath> = members.into_iter().collect();
        if members.is_empty() {
            return Self::Unnamed;
        }
        Self::ByName { verb, members }
    }
}

/// One member's pin moved by name, as a commit records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repin {
    pub(crate) member: RepoPath,
    pub(crate) from: Option<CommitId>,
    pub(crate) to: CommitId,
    pub(crate) verb: String,
}

impl Repin {
    /// `"<member>" <old> <new> <verb>`: the member a JSON string, so any
    /// path reads back whole; each pin a full commit id, the old one `none`
    /// where there was none; the verb the rest of the line.
    fn value(&self) -> String {
        format!(
            "{} {} {} {}",
            serde_json::Value::String(self.member.as_str().to_owned()),
            self.from.as_ref().map_or(NO_PIN, CommitId::as_str),
            self.to.as_str(),
            self.verb
        )
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        let mut quoted = serde_json::Deserializer::from_str(value).into_iter::<String>();
        let member = RepoPath::new(quoted.next()?.ok()?).ok()?;
        let rest = value[quoted.byte_offset()..].strip_prefix(' ')?;
        let mut fields = rest.splitn(3, ' ');
        let from = match fields.next()? {
            NO_PIN => None,
            id => Some(CommitId::parse(id).ok()?),
        };
        let to = CommitId::parse(fields.next()?).ok()?;
        let verb = fields.next()?.trim();
        if verb.is_empty() {
            return None;
        }
        Some(Self {
            member,
            from,
            to,
            verb: verb.to_owned(),
        })
    }

    fn annotation(&self) -> Annotation {
        Annotation {
            key: KEY,
            value: self.value(),
        }
    }
}

fn pins_of(lock: Option<&LockFile>) -> BTreeMap<RepoPath, CommitId> {
    lock.into_iter()
        .flat_map(LockFile::iter_entries)
        .filter_map(|(path, entry)| Some((path.clone(), entry.version.as_commit_id()?)))
        .collect()
}

fn lock_at(vcs: &dyn Vcs, dir: &Path, at: &ResolvedRevisionId) -> Option<LockFile> {
    let text = vcs
        .read_file_at_revision(dir, at, Path::new(LockFile::FILE_NAME))
        .ok()?;
    LockFile::from_json_str(&text).ok()
}

/// The re-pins `naming` makes, from the pins `parent`'s lock holds to those
/// `lock` holds.
fn repins(
    vcs: &dyn Vcs,
    dir: &Path,
    parent: Option<ResolvedRevisionId>,
    lock: &LockFile,
    naming: &Naming,
) -> Vec<Annotation> {
    let Naming::ByName { verb, members } = naming else {
        return Vec::new();
    };
    let before = pins_of(
        parent
            .and_then(|parent| lock_at(vcs, dir, &parent))
            .as_ref(),
    );
    let after = pins_of(Some(lock));
    members
        .iter()
        .filter_map(|member| {
            let to = after.get(member)?;
            let from = before.get(member);
            (from != Some(to)).then(|| {
                Repin {
                    member: member.clone(),
                    from: from.cloned(),
                    to: to.clone(),
                    verb: (*verb).to_owned(),
                }
                .annotation()
            })
        })
        .collect()
}

/// Commit `paths` of the project repo at `dir`, whose `rwv.lock` stages
/// `lock`, recording each pin of a member `naming` names that moves from the
/// one `HEAD`'s lock holds.
pub(crate) fn commit(
    vcs: &dyn Vcs,
    dir: &Path,
    paths: &[&str],
    message: &str,
    lock: Option<&LockFile>,
    naming: &Naming,
) -> Result<bool, VcsError> {
    commit_annotated(vcs, dir, paths, message, lock, naming, Vec::new())
}

/// [`commit`], recording `annotations` too, after the re-pins.
pub(crate) fn commit_annotated(
    vcs: &dyn Vcs,
    dir: &Path,
    paths: &[&str],
    message: &str,
    lock: Option<&LockFile>,
    naming: &Naming,
    annotations: Vec<Annotation>,
) -> Result<bool, VcsError> {
    let mut recorded = match lock {
        Some(lock) => repins(vcs, dir, vcs.head_revision(dir).ok(), lock, naming),
        None => Vec::new(),
    };
    recorded.extend(annotations);
    vcs.commit_only(dir, paths, message, &recorded)
}

/// Commit the merge staged in `dir`, whose `rwv.lock` stages `lock`,
/// recording each pin of a member `naming` names that moves from the one
/// `first`'s lock holds.
pub(crate) fn commit_merge(
    vcs: &dyn Vcs,
    dir: &Path,
    (first, second): (&CommitId, &CommitId),
    message: &str,
    lock: Option<&LockFile>,
    naming: &Naming,
) -> Result<CommitId, VcsError> {
    let annotations = match lock {
        Some(lock) => repins(
            vcs,
            dir,
            Some(ResolvedRevisionId::from(first.clone())),
            lock,
            naming,
        ),
        None => Vec::new(),
    };
    vcs.commit_merge(dir, first, second, message, &annotations)
}

/// The re-pins `commit` records; a value that does not read as one is left
/// out.
pub(crate) fn recorded(
    vcs: &dyn Vcs,
    dir: &Path,
    commit: &CommitId,
) -> Result<Vec<Repin>, VcsError> {
    Ok(vcs
        .annotation_values(dir, commit, KEY)?
        .iter()
        .filter_map(|value| Repin::parse(value))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(fill: char) -> CommitId {
        CommitId::parse(&fill.to_string().repeat(40)).unwrap()
    }

    #[test]
    fn a_member_path_holding_slashes_spaces_and_quotes_reads_back_whole() {
        for member in ["github/o/lib", "deps/with space/x", "odd\"name/x", "a"] {
            let Ok(member) = RepoPath::new(member) else {
                continue;
            };
            for from in [None, Some(id('a'))] {
                let repin = Repin {
                    member: member.clone(),
                    from,
                    to: id('b'),
                    verb: "member set-branch".to_owned(),
                };
                assert_eq!(Repin::parse(&repin.value()), Some(repin));
            }
        }
    }

    #[test]
    fn a_value_missing_a_field_or_holding_a_short_id_reads_as_nothing() {
        let full = id('c');
        for value in [
            String::new(),
            format!("\"m\" none {}", full.as_str()),
            format!("\"m\" none {} ", full.as_str()),
            format!("\"m\" abc {} lock", full.as_str()),
            format!("\"m\"none {} lock", full.as_str()),
            format!("m none {} lock", full.as_str()),
        ] {
            assert_eq!(Repin::parse(&value), None, "{value}");
        }
    }
}
