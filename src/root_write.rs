//! A real file at a path an ecosystem tool writes through its weave-root link.
//!
//! The link exists before the file so a tool's first write lands in the
//! project directory, and the tools measured keep it: they write in place
//! through it, or resolve it and rename over its target. Two things still
//! leave a real file at the root: a tool that renames a temporary file over a
//! link that dangles, and any tool writing where the link is missing. That
//! file is the tool's output, not an occupant. A verb that may write takes it
//! into the project directory and puts the link back; one that may not
//! reports it.

use crate::manifest::ProjectName;
use crate::owned_state::{owned_digest, recorded_digest};
use crate::workspace::project_dir;
use std::path::{Path, PathBuf};

/// Whether a root file can be taken into the project without losing the
/// project's own copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The project has no copy, the same bytes, or still the bytes the ledger
    /// records rwv accepted.
    Adopt,
    /// The project's copy moved since rwv recorded it, or nothing records it,
    /// and it differs from the root file.
    Conflict,
}

pub(crate) fn verdict(root: &[u8], project: Option<&[u8]>, recorded: Option<&str>) -> Verdict {
    match project {
        None => Verdict::Adopt,
        Some(project) if project == root => Verdict::Adopt,
        Some(project) if recorded == Some(owned_digest(project).as_str()) => Verdict::Adopt,
        Some(_) => Verdict::Conflict,
    }
}

/// A real file found at `name` under the weave root, read against the project
/// it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootWrite {
    pub name: String,
    pub owner: ProjectName,
    pub bytes: Vec<u8>,
    pub verdict: Verdict,
    /// Whether the ledger records a generation of this lock, which is what
    /// tells a project copy that moved from one that did not.
    pub recorded: bool,
}

/// The real file at `name`, if that is what stands there, judged against
/// `owner`'s copy. A link, a directory or nothing is not a root write.
pub(crate) fn read(
    root: &Path,
    owner: &ProjectName,
    name: &str,
) -> std::io::Result<Option<RootWrite>> {
    let path = root.join(name);
    match path.symlink_metadata() {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let bytes = std::fs::read(&path)?;
    let dir = project_dir(root, owner.as_str());
    let project = match std::fs::read(dir.join(name)) {
        Ok(project) => Some(project),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let recorded = recorded_digest(&dir, name);
    let verdict = verdict(&bytes, project.as_deref(), recorded.as_deref());
    Ok(Some(RootWrite {
        name: name.to_string(),
        owner: owner.clone(),
        bytes,
        verdict,
        recorded: recorded.is_some(),
    }))
}

impl RootWrite {
    /// The file changes that take this root file into its project: the
    /// project copy takes its bytes, and the root file goes so the link can
    /// stand there again.
    pub(crate) fn adoption(&self, root: &Path) -> Vec<Op> {
        let dest = project_dir(root, self.owner.as_str()).join(&self.name);
        let mut ops = Vec::new();
        if std::fs::read(&dest).ok().as_deref() != Some(self.bytes.as_slice()) {
            ops.push(Op::Write {
                path: dest,
                bytes: self.bytes.clone(),
            });
        }
        ops.push(Op::Remove {
            path: root.join(&self.name),
        });
        ops
    }

    fn in_project(&self) -> String {
        format!(
            "{}/{}",
            crate::workspace::project_rel_path(self.owner.as_str()),
            self.name
        )
    }

    /// What a verb that may not write says about it.
    pub(crate) fn unadopted_message(&self) -> String {
        format!(
            "`{}` at the weave root is a real file a tool wrote where its link belongs, not \
             yet taken into `{}`; its link is left out until it is. Run `rwv doctor --fix` \
             to adopt it",
            self.name,
            self.in_project()
        )
    }

    /// What is said when the root file cannot be taken over the project's copy.
    pub(crate) fn conflict_message(&self) -> String {
        let why = if self.recorded {
            "both changed since rwv last recorded the lock"
        } else {
            "differ, and rwv records no generation of this lock to tell which one changed"
        };
        format!(
            "`{name}` at the weave root and `{project}` {why}, so neither is taken over the \
             other and the link is left out. To keep the root's, copy it over `{project}`; \
             to keep the project's, delete `{name}` at the weave root. Then re-run `rwv \
             doctor --fix`",
            name = self.name,
            project = self.in_project()
        )
    }
}

/// One file change, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
    Write { path: PathBuf, bytes: Vec<u8> },
    Remove { path: PathBuf },
}

/// What a path held before a change.
#[derive(Debug)]
enum Prior {
    Absent,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Every path a pass changed, with what it held first, so a pass that fails
/// partway can put each one back.
#[derive(Debug, Default)]
pub(crate) struct FileJournal {
    priors: Vec<(PathBuf, Prior)>,
}

impl FileJournal {
    fn note(&mut self, path: &Path) -> anyhow::Result<()> {
        if self.priors.iter().any(|(noted, _)| noted == path) {
            return Ok(());
        }
        let prior = match path.symlink_metadata() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Prior::Absent,
            Err(e) => return Err(e.into()),
            Ok(meta) if meta.file_type().is_symlink() => Prior::Link(std::fs::read_link(path)?),
            Ok(_) => Prior::File(std::fs::read(path)?),
        };
        self.priors.push((path.to_path_buf(), prior));
        Ok(())
    }

    /// Carry out `ops`, noting what each path held before touching it.
    pub(crate) fn apply(&mut self, ops: &[Op]) -> anyhow::Result<()> {
        for op in ops {
            match op {
                Op::Write { path, bytes } => {
                    self.note(path)?;
                    crate::durable_file::replace(path, bytes)?;
                }
                Op::Remove { path } => {
                    self.note(path)?;
                    remove_entry(path)?;
                }
            }
        }
        Ok(())
    }

    /// Put every noted path back as it was, newest change first. Returns the
    /// paths that could not be.
    pub(crate) fn roll_back(self) -> Vec<PathBuf> {
        let mut unrestored = Vec::new();
        for (path, prior) in self.priors.into_iter().rev() {
            let restored = match prior {
                Prior::Absent => remove_entry(&path).or_else(absent_is_done).is_ok(),
                Prior::File(bytes) => crate::durable_file::replace(&path, &bytes).is_ok(),
                Prior::Link(target) => {
                    let kind = crate::symlink::LinkTarget::on_disk(
                        &path.parent().unwrap_or(Path::new("")).join(&target),
                    );
                    remove_entry(&path).or_else(absent_is_done).is_ok()
                        && crate::symlink::create(&target, &path, kind).is_ok()
                }
            };
            if !restored {
                unrestored.push(path);
            }
        }
        unrestored
    }
}

fn absent_is_done(e: std::io::Error) -> std::io::Result<()> {
    if e.kind() == std::io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(e)
    }
}

/// Unlink a file or a link by what it is.
fn remove_entry(path: &Path) -> std::io::Result<()> {
    if path.symlink_metadata()?.file_type().is_symlink() {
        crate::symlink::remove(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_write_is_adopted_when_the_project_loses_nothing() {
        assert_eq!(verdict(b"new", None, None), Verdict::Adopt);
        assert_eq!(verdict(b"same", Some(b"same"), None), Verdict::Adopt);
        assert_eq!(
            verdict(b"new", Some(b"accepted"), Some(&owned_digest(b"accepted"))),
            Verdict::Adopt,
            "the project still holds what rwv recorded, so nothing of its own is lost"
        );
    }

    #[test]
    fn a_conflict_says_whether_anything_records_which_side_moved() {
        let write = |recorded| RootWrite {
            name: "package-lock.json".to_string(),
            owner: ProjectName::new("demo").unwrap(),
            bytes: Vec::new(),
            verdict: Verdict::Conflict,
            recorded,
        };
        assert!(write(true).conflict_message().contains("both changed"));
        assert!(
            write(false)
                .conflict_message()
                .contains("records no generation of this lock"),
            "with nothing recorded, rwv cannot say the project's copy changed"
        );
    }

    #[test]
    fn a_root_write_over_a_project_copy_that_moved_is_a_conflict() {
        assert_eq!(
            verdict(b"new", Some(b"pulled"), Some(&owned_digest(b"accepted"))),
            Verdict::Conflict
        );
        assert_eq!(
            verdict(b"new", Some(b"unrecorded"), None),
            Verdict::Conflict,
            "with nothing recorded there is no telling which side moved"
        );
    }
}
