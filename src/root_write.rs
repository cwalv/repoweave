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
use crate::vcs::{Vcs, VcsError};
use crate::workspace::project_dir;
use std::path::{Path, PathBuf};

/// Whether a root file can be taken into the project without losing the
/// project's own copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The project has no copy, the same bytes, the generation the ledger
    /// records rwv accepted, or a copy its last commit holds as it stands.
    Adopt,
    /// The project's copy differs, and neither the ledger nor version control
    /// holds it.
    Conflict,
}

/// A real file found at `name` under the weave root, with the project copy
/// taking it in would replace.
#[derive(Debug, Clone)]
pub(crate) struct RootWrite {
    pub name: String,
    pub owner: ProjectName,
    pub bytes: Vec<u8>,
    project_copy: Option<Vec<u8>>,
    recorded: Option<String>,
}

/// The real file at `name`, if that is what stands there. A link, a directory
/// or nothing is not a root write.
pub(crate) fn read(
    root: &Path,
    owner: &ProjectName,
    name: &str,
) -> std::io::Result<Option<RootWrite>> {
    let path = root.join(crate::path_spelling::native_relative(name));
    match path.symlink_metadata() {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let bytes = std::fs::read(&path)?;
    let dir = project_dir(root, owner);
    let project_copy = match std::fs::read(dir.join(crate::path_spelling::native_relative(name))) {
        Ok(copy) => Some(copy),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    Ok(Some(RootWrite {
        name: name.to_string(),
        owner: owner.clone(),
        bytes,
        project_copy,
        recorded: recorded_digest(&dir, name),
    }))
}

impl RootWrite {
    /// Adopt when taking this file in loses nothing: the project has no copy,
    /// the same bytes, the generation rwv recorded, or a copy `vcs` can give
    /// back from its last commit.
    pub(crate) fn verdict(&self, vcs: &dyn Vcs, root: &Path) -> Result<Verdict, VcsError> {
        let replaceable = match &self.project_copy {
            None => true,
            Some(copy) if *copy == self.bytes => true,
            Some(copy) if self.recorded.as_deref() == Some(owned_digest(copy).as_str()) => true,
            Some(_) => {
                vcs.holds_committed_content(&project_dir(root, &self.owner), Path::new(&self.name))?
            }
        };
        Ok(if replaceable {
            Verdict::Adopt
        } else {
            Verdict::Conflict
        })
    }

    /// The file changes that take this root file into its project: the
    /// project copy takes its bytes, and the root file goes so the link can
    /// stand there again.
    pub(crate) fn adoption(&self, root: &Path) -> Vec<Op> {
        let dest =
            project_dir(root, &self.owner).join(crate::path_spelling::native_relative(&self.name));
        let mut ops = Vec::new();
        if std::fs::read(&dest).ok().as_deref() != Some(self.bytes.as_slice()) {
            ops.push(Op::Write {
                path: dest,
                bytes: self.bytes.clone(),
            });
        }
        ops.push(Op::Remove {
            path: root.join(crate::path_spelling::native_relative(&self.name)),
        });
        ops
    }

    fn in_project(&self) -> String {
        format!(
            "{}/{}",
            crate::workspace::project_rel_path(&self.owner),
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

    fn conflict_reason(&self) -> String {
        format!(
            "`{name}` at the weave root and `{project}` differ, and neither version control nor \
             rwv's record holds the project's copy as it stands",
            name = self.name,
            project = self.in_project()
        )
    }

    fn conflict_remedy(&self) -> String {
        format!(
            "To keep the root's, commit the project's copy or delete it; to keep the project's, \
             delete `{}` at the weave root",
            self.name
        )
    }

    /// What is said when the root file cannot be taken over the project's copy.
    pub(crate) fn conflict_message(&self) -> String {
        format!(
            "{}, so neither is taken over the other and the link is left out. {}. Then re-run \
             `rwv doctor --fix`",
            self.conflict_reason(),
            self.conflict_remedy()
        )
    }

    /// Why selecting `incoming` in place of this file's owner is refused.
    pub(crate) fn switch_refusal_message(&self, incoming: &ProjectName) -> String {
        format!(
            "{reason}; switching would leave `{name}` at the root for whichever project next \
             surfaces that name to take in. {remedy}. Then run `rwv doctor --fix` and re-run \
             `rwv activate {incoming}`",
            reason = self.conflict_reason(),
            name = self.name,
            remedy = self.conflict_remedy(),
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

    fn git(repo: &Path, args: &[&str]) {
        let out = crate::git::git_command_in_test()
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(repo: &Path) {
        git(
            repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=T",
                "commit",
                "-qm",
                "c",
            ],
        );
    }

    fn commit_lock(repo: &Path, bytes: &str) {
        std::fs::write(repo.join("Cargo.lock"), bytes).unwrap();
        git(repo, &["add", "Cargo.lock"]);
        commit(repo);
    }

    /// The verdict on a root `Cargo.lock` over a project repo `stage` left in
    /// some state.
    fn verdict_over(stage: impl FnOnce(&Path)) -> Verdict {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let repo = root.join("projects/demo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("rwv.toml"), "").unwrap();
        git(&repo, &["add", "rwv.toml"]);
        commit(&repo);
        stage(&repo);
        std::fs::write(root.join("Cargo.lock"), "# the tool's\n").unwrap();
        read(root, &ProjectName::new("demo").unwrap(), "Cargo.lock")
            .unwrap()
            .unwrap()
            .verdict(crate::vcs::project_vcs().as_ref(), root)
            .unwrap()
    }

    #[test]
    fn a_root_write_is_adopted_when_the_project_loses_nothing() {
        assert_eq!(verdict_over(|_| {}), Verdict::Adopt, "no project copy");
        assert_eq!(
            verdict_over(|repo| std::fs::write(repo.join("Cargo.lock"), "# the tool's\n").unwrap()),
            Verdict::Adopt,
            "the same bytes, though version control holds none of them"
        );
        assert_eq!(
            verdict_over(|repo| {
                std::fs::write(repo.join("Cargo.lock"), "# accepted\n").unwrap();
                crate::owned_state::stamp_owned_digest(repo, "Cargo.lock", b"# accepted\n")
                    .unwrap();
            }),
            Verdict::Adopt,
            "the generation rwv recorded, though version control holds none of it"
        );
        assert_eq!(
            verdict_over(|repo| commit_lock(repo, "# committed\n")),
            Verdict::Adopt,
            "a copy history gives back"
        );
        assert_eq!(
            verdict_over(|repo| {
                commit_lock(repo, "# committed\n");
                std::fs::write(repo.join("rwv.toml"), "# edited\n").unwrap();
            }),
            Verdict::Adopt,
            "another file's edit does not make the lock unrecoverable"
        );
    }

    #[test]
    fn a_root_write_over_a_copy_history_cannot_give_back_is_a_conflict() {
        assert_eq!(
            verdict_over(|repo| std::fs::write(repo.join("Cargo.lock"), "# untracked\n").unwrap()),
            Verdict::Conflict,
            "untracked"
        );
        assert_eq!(
            verdict_over(|repo| {
                std::fs::write(repo.join("Cargo.lock"), "# moved\n").unwrap();
                crate::owned_state::stamp_owned_digest(repo, "Cargo.lock", b"# accepted\n")
                    .unwrap();
            }),
            Verdict::Conflict,
            "untracked, and moved since rwv recorded it"
        );
        assert_eq!(
            verdict_over(|repo| {
                commit_lock(repo, "# committed\n");
                std::fs::write(repo.join("Cargo.lock"), "# edited\n").unwrap();
            }),
            Verdict::Conflict,
            "edited since its commit"
        );
        assert_eq!(
            verdict_over(|repo| {
                commit_lock(repo, "# committed\n");
                std::fs::write(repo.join("Cargo.lock"), "# staged\n").unwrap();
                git(repo, &["add", "Cargo.lock"]);
            }),
            Verdict::Conflict,
            "staged, which no commit holds"
        );
    }

    #[test]
    fn a_differing_copy_in_a_project_outside_version_control_is_a_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let project = root.join("projects/demo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("Cargo.lock"), "# the project's\n").unwrap();
        std::fs::write(root.join("Cargo.lock"), "# the tool's\n").unwrap();

        let verdict = read(root, &ProjectName::new("demo").unwrap(), "Cargo.lock")
            .unwrap()
            .unwrap()
            .verdict(crate::vcs::project_vcs().as_ref(), root);

        assert_eq!(verdict.unwrap(), Verdict::Conflict);
    }

    fn hidden_edit_under(flag: &str) -> Verdict {
        verdict_over(|repo| {
            commit_lock(repo, "# committed\n");
            git(repo, &["update-index", flag, "Cargo.lock"]);
            std::fs::write(repo.join("Cargo.lock"), "# hidden edit\n").unwrap();
        })
    }

    #[test]
    fn an_edit_hidden_by_assume_unchanged_is_a_conflict() {
        assert_eq!(hidden_edit_under("--assume-unchanged"), Verdict::Conflict);
    }

    #[test]
    fn an_edit_hidden_by_skip_worktree_is_a_conflict() {
        assert_eq!(hidden_edit_under("--skip-worktree"), Verdict::Conflict);
    }

    #[test]
    fn an_edited_staged_rename_target_is_a_conflict() {
        assert_eq!(
            verdict_over(|repo| {
                std::fs::write(repo.join("old.lock"), "# committed\n").unwrap();
                git(repo, &["add", "old.lock"]);
                commit(repo);
                git(repo, &["mv", "old.lock", "Cargo.lock"]);
                std::fs::write(repo.join("Cargo.lock"), "# edited on top\n").unwrap();
            }),
            Verdict::Conflict
        );
    }

    #[test]
    fn a_hiding_flag_over_the_committed_bytes_is_adopted() {
        assert_eq!(
            verdict_over(|repo| {
                commit_lock(repo, "# committed\n");
                git(repo, &["update-index", "--assume-unchanged", "Cargo.lock"]);
            }),
            Verdict::Adopt
        );
    }
}
