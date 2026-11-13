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

    pub(crate) fn annotation(&self) -> Annotation {
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
/// one `HEAD`'s lock holds. Refused while an operation is in flight there.
pub(crate) fn commit(
    vcs: &dyn Vcs,
    dir: &Path,
    paths: &[&str],
    message: &str,
    lock: Option<&LockFile>,
    naming: &Naming,
) -> anyhow::Result<bool> {
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
) -> anyhow::Result<bool> {
    crate::commit_guard::refuse_commit_mid_operation(vcs, dir.display(), dir, None)?;
    commit_unguarded(vcs, dir, paths, message, lock, naming, annotations)
}

/// [`commit_annotated`] for the commit that concludes `copy`, the one
/// operation in flight that commit may conclude.
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit_concluding_copy(
    vcs: &dyn Vcs,
    dir: &Path,
    paths: &[&str],
    message: &str,
    lock: Option<&LockFile>,
    naming: &Naming,
    annotations: Vec<Annotation>,
    copy: crate::commit_guard::ConcludingCopy<'_>,
) -> anyhow::Result<bool> {
    crate::commit_guard::refuse_commit_mid_operation_but(vcs, dir.display(), dir, copy)?;
    commit_unguarded(vcs, dir, paths, message, lock, naming, annotations)
}

fn commit_unguarded(
    vcs: &dyn Vcs,
    dir: &Path,
    paths: &[&str],
    message: &str,
    lock: Option<&LockFile>,
    naming: &Naming,
    annotations: Vec<Annotation>,
) -> anyhow::Result<bool> {
    let mut recorded = match lock {
        Some(lock) => repins(vcs, dir, vcs.head_revision(dir).ok(), lock, naming),
        None => Vec::new(),
    };
    recorded.extend(annotations);
    Ok(vcs.commit_only(dir, paths, message, &recorded)?)
}

/// Commit the merge staged in `dir`, whose `rwv.lock` stages `lock`,
/// recording each pin of a member `naming` names that moves from the one
/// `first`'s lock holds.
pub(crate) fn commit_merge(
    vcs: &dyn Vcs,
    dir: &Path,
    parents: (&CommitId, &CommitId),
    message: &str,
    lock: Option<&LockFile>,
    naming: &Naming,
) -> Result<CommitId, VcsError> {
    commit_merge_annotated(vcs, dir, parents, message, lock, naming, Vec::new())
}

/// [`commit_merge`], recording `extra` too, after the re-pins.
pub(crate) fn commit_merge_annotated(
    vcs: &dyn Vcs,
    dir: &Path,
    (first, second): (&CommitId, &CommitId),
    message: &str,
    lock: Option<&LockFile>,
    naming: &Naming,
    extra: Vec<Annotation>,
) -> Result<CommitId, VcsError> {
    let mut annotations = match lock {
        Some(lock) => repins(
            vcs,
            dir,
            Some(ResolvedRevisionId::from(first.clone())),
            lock,
            naming,
        ),
        None => Vec::new(),
    };
    annotations.extend(extra);
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

/// A copy of the replayed `commit` on `parent` whose `rwv.lock` is `lock`,
/// its message kept, each re-pin it records taken through `map` as its
/// lock's pins were; a re-pin `map` leaves moving nothing, or moving off a
/// pin `parent`'s lock does not hold, is dropped.
pub(crate) fn copy_with_lock(
    vcs: &dyn Vcs,
    dir: &Path,
    commit: &CommitId,
    parent: &CommitId,
    lock: Vec<u8>,
    map: &mut dyn FnMut(&RepoPath, &CommitId) -> CommitId,
) -> Result<CommitId, VcsError> {
    let records = recorded(vcs, dir, commit)?;
    let had_records = !records.is_empty();
    let parent_lock = if had_records {
        vcs.read_file_at_revision(
            dir,
            &ResolvedRevisionId::from(parent.clone()),
            Path::new(LockFile::FILE_NAME),
        )
        .ok()
        .and_then(|text| LockFile::from_json_str(&text).ok())
    } else {
        None
    };
    let held_by_parent = |member: &RepoPath, pin: &CommitId| {
        parent_lock
            .as_ref()
            .and_then(|lock| lock.get_entry(member))
            .and_then(|entry| entry.version.as_commit_id())
            .as_ref()
            == Some(pin)
    };
    let mapped: Vec<Annotation> = records
        .into_iter()
        .filter_map(|repin| {
            let from = repin.from.as_ref().map(|from| map(&repin.member, from));
            let to = map(&repin.member, &repin.to);
            let moves = from.as_ref() != Some(&to)
                && from
                    .as_ref()
                    .is_none_or(|from| held_by_parent(&repin.member, from));
            moves.then(|| Repin { from, to, ..repin }.annotation())
        })
        .collect();
    vcs.copy_replayed(
        dir,
        commit,
        parent,
        &[crate::vcs::FileChange {
            path: LockFile::FILE_NAME.to_owned(),
            content: Some(lock),
        }],
        had_records.then_some((KEY, mapped.as_slice())),
    )
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

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn copied_records(parent_pin: CommitId, map_to: impl Fn(&CommitId) -> CommitId) -> Vec<Repin> {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(
            dir.join(LockFile::FILE_NAME),
            format!(
                r#"{{"repositories":{{"m":{{"type":"git","url":"u","version":"{}"}}}}}}"#,
                parent_pin.as_str()
            ),
        )
        .unwrap();
        git(dir, &["add", LockFile::FILE_NAME]);
        git(dir, &["commit", "-q", "-m", "base"]);
        let parent = CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap();
        let (from, to) = (id('a'), id('b'));
        let message = format!(
            "lock\n\n{KEY}: \"m\" {} {} member pin",
            from.as_str(),
            to.as_str()
        );
        git(dir, &["commit", "-q", "--allow-empty", "-m", &message]);
        let commit = CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap();
        let vcs = crate::git::git_vcs();
        let copy = copy_with_lock(
            vcs.as_ref(),
            dir,
            &commit,
            &parent,
            b"{}".to_vec(),
            &mut |_, pin| map_to(pin),
        )
        .unwrap();
        recorded(vcs.as_ref(), dir, &copy).unwrap()
    }

    #[test]
    fn a_copied_record_takes_both_its_pins_through_the_lock_s_re_map() {
        let records = copied_records(
            id('c'),
            |pin| {
                if *pin == id('a') {
                    id('c')
                } else {
                    id('d')
                }
            },
        );
        assert_eq!(
            records
                .iter()
                .map(|r| (r.from.clone(), r.to.clone()))
                .collect::<Vec<_>>(),
            vec![(Some(id('c')), id('d'))]
        );
    }

    #[test]
    fn a_copied_record_the_re_map_leaves_moving_nothing_is_dropped() {
        assert!(copied_records(id('e'), |_| id('e')).is_empty());
    }

    #[test]
    fn a_copied_record_moving_off_a_pin_its_new_parent_does_not_hold_is_dropped() {
        let records = copied_records(
            id('f'),
            |pin| {
                if *pin == id('a') {
                    id('c')
                } else {
                    id('d')
                }
            },
        );
        assert!(records.is_empty());
    }
}
