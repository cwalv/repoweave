//! Placing a member's tracked files by copy-on-write clone from the
//! repository's other workspaces, so a workspace costs disk only where its
//! files differ from one that already holds them.
//!
//! The backend says what a checkout writes, which source files still hold
//! exactly that, and writes whatever was not placed; nothing here knows how a
//! VCS stores content. Nothing records which files were cloned: a later
//! operation asks the backend again.

use crate::vcs::{
    BornRef, OwnedRef, ResolvedRevisionId, TrackedFile, Vcs, VcsError, WorkspacePopulation,
};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// How a member's tracked files reached a workspace.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilePlacement {
    /// Cloned from another workspace's copy, sharing its storage until either
    /// is written.
    pub reflinked: usize,
    /// Linked to another workspace's read-only copy.
    pub hardlinked: usize,
    /// Written by the version control system.
    pub written: usize,
}

impl fmt::Display for FilePlacement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} reflinked, {} hardlinked, {} written",
            self.reflinked, self.hardlinked, self.written
        )
    }
}

/// ` (N reflinked, N hardlinked, N written)` for a line naming a member, or
/// nothing where the backend could not say.
pub(crate) fn files_note(placement: Option<FilePlacement>) -> String {
    placement.map(|p| format!(" ({p})")).unwrap_or_default()
}

/// The filesystem operations placement needs, apart from the logic choosing
/// them so a test can fail them on demand.
pub(crate) trait FilePrimitives: Sync {
    /// Make `to`, which must not exist, a copy-on-write clone of `from`.
    fn reflink(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// The filesystem this process runs on.
pub(crate) struct Filesystem;

impl FilePrimitives for Filesystem {
    fn reflink(&self, from: &Path, to: &Path) -> io::Result<()> {
        reflink_copy::reflink(from, to)
    }
}

/// An error saying that nothing at all can be cloned between these two
/// places, as opposed to this one file failing.
fn cannot_clone_between(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::CrossesDevices | io::ErrorKind::InvalidInput
    )
}

/// What creating a workspace with shared files came to.
pub(crate) struct Populated {
    /// What [`Vcs::create_worktree_on`] would have answered: `Some` iff this
    /// call authored the ref.
    pub(crate) created: Result<Option<BornRef>, VcsError>,
    /// How the workspace's files were placed once it existed. `Ok(None)` for a
    /// backend that answers no population questions, or a creation that
    /// failed.
    pub(crate) filled: anyhow::Result<Option<FilePlacement>>,
}

/// Create the workspace [`Vcs::create_worktree_on`] would, with every file
/// another workspace of the repository holds cloned from it and the rest
/// written by the backend.
pub(crate) fn create_worktree_populated(
    vcs: &dyn Vcs,
    owned: &OwnedRef,
    dest: &Path,
    fs: &dyn FilePrimitives,
) -> Populated {
    let Some(population) = vcs.population() else {
        return Populated {
            created: vcs.create_worktree_on(owned, dest),
            filled: Ok(None),
        };
    };
    let created = population.create_unpopulated_worktree_on(owned, dest);
    let filled = match &created {
        Ok(_) => fill(vcs, population, dest, owned.created_at(), fs).map(Some),
        Err(_) => Ok(None),
    };
    Populated { created, filled }
}

fn fill(
    vcs: &dyn Vcs,
    population: &dyn WorkspacePopulation,
    dest: &Path,
    at: &ResolvedRevisionId,
    fs: &dyn FilePrimitives,
) -> anyhow::Result<FilePlacement> {
    let tracked = population.tracked_files(dest)?;
    let wanted: Vec<&TrackedFile> = tracked.iter().filter(|f| f.identity().is_some()).collect();
    let placed = place(vcs, population, dest, at, &wanted, fs, Placing::IntoEmpty);
    population
        .finish_checkout(dest)
        .with_context(|| format!("failed to finish the checkout at {}", dest.display()))?;
    let reflinked = verify_and_repair(population, dest, &placed)?;
    Ok(FilePlacement {
        reflinked,
        hardlinked: 0,
        written: tracked.len() - reflinked,
    })
}

/// After the backend rewrote `changed` in the member checked out at
/// `member`, replace each rewritten file with a clone of another workspace's
/// copy of the same content. A file that no longer holds what the member
/// records is never replaced, so no local change is lost.
///
/// `None` for a backend that answers no population questions.
pub(crate) fn reshare(
    vcs: &dyn Vcs,
    member: &Path,
    changed: &[String],
    fs: &dyn FilePrimitives,
) -> anyhow::Result<Option<FilePlacement>> {
    let Some(population) = vcs.population() else {
        return Ok(None);
    };
    let changed: BTreeSet<&str> = changed.iter().map(String::as_str).collect();
    let rewritten: Vec<TrackedFile> = population
        .tracked_files(member)?
        .into_iter()
        .filter(|f| changed.contains(f.path.as_str()))
        .collect();
    let shareable: Vec<String> = rewritten
        .iter()
        .filter(|f| f.identity().is_some())
        .map(|f| f.path.clone())
        .collect();
    let intact: BTreeSet<String> = if shareable.is_empty() {
        BTreeSet::new()
    } else {
        population
            .files_holding_recorded_content(member, &shareable)?
            .into_iter()
            .map(|f| f.path)
            .collect()
    };
    let wanted: Vec<&TrackedFile> = rewritten
        .iter()
        .filter(|f| intact.contains(&f.path))
        .collect();
    let at = vcs.head_revision(member)?;
    let placed = place(
        vcs,
        population,
        member,
        &at,
        &wanted,
        fs,
        Placing::OverIntact,
    );
    let reflinked = verify_and_repair(population, member, &placed)?;
    Ok(Some(FilePlacement {
        reflinked,
        hardlinked: 0,
        written: rewritten.len() - reflinked,
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Placing {
    /// Nothing is at the path yet.
    IntoEmpty,
    /// The path holds what the workspace records; replace it whole.
    OverIntact,
}

/// Clone each of `wanted` into `dest` from a workspace holding its content,
/// and return the paths cloned. A file nothing can be cloned from is left
/// for the backend.
fn place(
    vcs: &dyn Vcs,
    population: &dyn WorkspacePopulation,
    dest: &Path,
    at: &ResolvedRevisionId,
    wanted: &[&TrackedFile],
    fs: &dyn FilePrimitives,
    placing: Placing,
) -> Vec<String> {
    let mut uncovered: BTreeMap<&str, &TrackedFile> =
        wanted.iter().map(|f| (f.path.as_str(), *f)).collect();
    let mut placed = Vec::new();
    for source in sources(vcs, dest, at) {
        if uncovered.is_empty() {
            break;
        }
        if !can_clone_from(&source, dest, uncovered.keys().copied(), fs) {
            continue;
        }
        let asked: Vec<String> = uncovered.keys().map(|p| (*p).to_owned()).collect();
        let Ok(held) = population.files_holding_recorded_content(&source, &asked) else {
            continue;
        };
        for offered in held {
            let Some(want) = uncovered.get(offered.path.as_str()).copied() else {
                continue;
            };
            if offered.kind != want.kind {
                continue;
            }
            match clone_into(&source, dest, &want.path, fs, placing) {
                Ok(()) => {
                    uncovered.remove(want.path.as_str());
                    placed.push(want.path.clone());
                }
                Err(e) if cannot_clone_between(&e) => break,
                Err(_) => {}
            }
        }
    }
    placed
}

/// Every other workspace of the repository `dest` belongs to, those at
/// `at` first and then the primary's checkout.
fn sources(vcs: &dyn Vcs, dest: &Path, at: &ResolvedRevisionId) -> Vec<PathBuf> {
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    let primary = vcs
        .resolve_canonical_store(dest)
        .and_then(|store| store.parent().map(Path::to_path_buf));
    let mut found: Vec<(bool, bool, PathBuf)> = vcs
        .list_worktrees(dest)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.is_dir() && !same(w, dest))
        .map(|w| {
            let elsewhere = vcs.head_revision(&w).map_or(true, |head| head != *at);
            let secondary = !primary.as_deref().is_some_and(|p| same(p, &w));
            (elsewhere, secondary, w)
        })
        .collect();
    found.sort_by_key(|(elsewhere, secondary, _)| (*elsewhere, *secondary));
    found.into_iter().map(|(_, _, w)| w).collect()
}

/// Whether anything can be cloned from `source` into `dest`, asked once by
/// cloning a file `source` has at one of `paths` and removing the clone, so
/// a filesystem that cannot clone costs one attempt rather than a read of
/// every source file.
fn can_clone_from<'a>(
    source: &Path,
    dest: &Path,
    mut paths: impl Iterator<Item = &'a str>,
    fs: &dyn FilePrimitives,
) -> bool {
    let Some(sample) = paths
        .find(|p| std::fs::symlink_metadata(source.join(p)).is_ok_and(|m| m.file_type().is_file()))
    else {
        return false;
    };
    let probe = dest.join(format!(".rwv-clone-probe-{}", std::process::id()));
    let cloned = fs.reflink(&source.join(sample), &probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    cloned
}

fn clone_into(
    source: &Path,
    dest: &Path,
    path: &str,
    fs: &dyn FilePrimitives,
    placing: Placing,
) -> io::Result<()> {
    let from = source.join(path);
    let to = dest.join(path);
    match placing {
        Placing::IntoEmpty => {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            fs.reflink(&from, &to)
        }
        Placing::OverIntact => {
            let name = to
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let staged = to.with_file_name(format!(".{name}.rwv-share"));
            let replaced = fs
                .reflink(&from, &staged)
                .and_then(|()| std::fs::rename(&staged, &to));
            if replaced.is_err() {
                let _ = std::fs::remove_file(&staged);
            }
            replaced
        }
    }
}

/// Check out again every one of `placed` that does not hold exactly what the
/// workspace records, and return how many did.
fn verify_and_repair(
    population: &dyn WorkspacePopulation,
    workspace: &Path,
    placed: &[String],
) -> anyhow::Result<usize> {
    if placed.is_empty() {
        return Ok(0);
    }
    let holding: BTreeSet<String> = population
        .files_holding_recorded_content(workspace, placed)?
        .into_iter()
        .map(|f| f.path)
        .collect();
    let mismatched: Vec<String> = placed
        .iter()
        .filter(|p| !holding.contains(*p))
        .cloned()
        .collect();
    population
        .check_out_paths(workspace, &mismatched)
        .with_context(|| format!("failed to repair placed files in {}", workspace.display()))?;
    Ok(placed.len() - mismatched.len())
}

#[cfg(test)]
pub(crate) mod testing {
    //! File primitives a test can fail on demand.

    use super::*;
    use std::sync::Mutex;

    type BeforeClone = Box<dyn FnMut(&Path, &Path) + Send>;

    /// Clones by copying, unless scripted to fail for a source.
    #[derive(Default)]
    pub(crate) struct FakePrimitives {
        failures: Mutex<Vec<(PathBuf, io::ErrorKind)>>,
        before_clone: Mutex<Option<BeforeClone>>,
        clones: Mutex<Vec<(PathBuf, PathBuf)>>,
        attempts: Mutex<usize>,
    }

    impl FakePrimitives {
        /// Every clone from under `source` fails with `kind`.
        pub(crate) fn fail_from(&self, source: &Path, kind: io::ErrorKind) {
            self.failures
                .lock()
                .unwrap()
                .push((source.to_path_buf(), kind));
        }

        /// Run `hook` with each clone's `(from, to)` just before it is made.
        pub(crate) fn before_clone(&self, hook: impl FnMut(&Path, &Path) + Send + 'static) {
            *self.before_clone.lock().unwrap() = Some(Box::new(hook));
        }

        /// Every clone that succeeded, as `(from, to)`.
        pub(crate) fn clones(&self) -> Vec<(PathBuf, PathBuf)> {
            self.clones.lock().unwrap().clone()
        }

        /// How many clones were asked for, failed ones included.
        pub(crate) fn attempts(&self) -> usize {
            *self.attempts.lock().unwrap()
        }
    }

    impl FilePrimitives for FakePrimitives {
        fn reflink(&self, from: &Path, to: &Path) -> io::Result<()> {
            *self.attempts.lock().unwrap() += 1;
            if let Some((_, kind)) = self
                .failures
                .lock()
                .unwrap()
                .iter()
                .find(|(source, _)| from.starts_with(source))
            {
                return Err(io::Error::from(*kind));
            }
            if to.symlink_metadata().is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            if let Some(hook) = self.before_clone.lock().unwrap().as_mut() {
                hook(from, to);
            }
            std::fs::copy(from, to)?;
            self.clones
                .lock()
                .unwrap()
                .push((from.to_path_buf(), to.to_path_buf()));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakePrimitives;
    use super::*;
    use crate::git::{git_command_in_test, git_vcs};
    use crate::vcs::{RawRefName, TrackedFileKind};

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = git_command_in_test()
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A canonical clone whose own checkout is the primary's.
    struct Weave {
        tmp: tempfile::TempDir,
        store: PathBuf,
    }

    impl Weave {
        fn with(files: &[(&str, &str)]) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let store = tmp.path().join("primary");
            std::fs::create_dir(&store).unwrap();
            git(&store, &["init", "-q", "-b", "main"]);
            let w = Self { tmp, store };
            w.commit(files);
            w
        }

        fn commit(&self, files: &[(&str, &str)]) -> ResolvedRevisionId {
            for (path, body) in files {
                let at = self.store.join(path);
                std::fs::create_dir_all(at.parent().unwrap()).unwrap();
                std::fs::write(at, body).unwrap();
            }
            git(&self.store, &["add", "."]);
            git(&self.store, &["commit", "-q", "-m", "c"]);
            self.head()
        }

        fn head(&self) -> ResolvedRevisionId {
            git_vcs().head_revision(&self.store).unwrap()
        }

        /// A workspace git checked out itself.
        fn seat(&self, name: &str, at: &ResolvedRevisionId) -> PathBuf {
            let dest = self.tmp.path().join(name);
            git(
                &self.store,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    name,
                    dest.to_str().unwrap(),
                    at.as_str(),
                ],
            );
            dest
        }

        fn populate(
            &self,
            name: &str,
            at: &ResolvedRevisionId,
            fs: &dyn FilePrimitives,
        ) -> (PathBuf, FilePlacement) {
            let dest = self.tmp.path().join(name);
            let owned =
                OwnedRef::from_receipt(self.store.join(".git"), RawRefName::new(name), at.clone());
            let populated = create_worktree_populated(git_vcs().as_ref(), &owned, &dest, fs);
            assert!(populated.created.unwrap().is_some());
            (
                dest,
                populated.filled.unwrap().expect("git answers population"),
            )
        }
    }

    fn files_in(dir: &Path) -> Vec<String> {
        git(dir, &["ls-files"]).lines().map(str::to_owned).collect()
    }

    /// Every tracked file holds exactly what a checkout writes.
    fn assert_checked_out(dest: &Path) {
        let population = git_vcs();
        let population = population.population().unwrap();
        let tracked = population.tracked_files(dest).unwrap();
        let shareable: Vec<String> = tracked
            .iter()
            .filter(|f| f.identity().is_some())
            .map(|f| f.path.clone())
            .collect();
        let holding = population
            .files_holding_recorded_content(dest, &shareable)
            .unwrap();
        assert_eq!(
            holding.len(),
            shareable.len(),
            "{shareable:?} vs {holding:?}"
        );
        assert_eq!(git(dest, &["status", "--porcelain"]), "");
    }

    /// The clones that placed a file, as `(source workspace, path)`; probes
    /// are left out.
    fn placements(fs: &FakePrimitives, candidates: &[&Path]) -> Vec<(PathBuf, String)> {
        fs.clones()
            .into_iter()
            .filter(|(_, to)| {
                !to.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".rwv-clone-probe")
            })
            .map(|(from, _)| {
                let source = candidates
                    .iter()
                    .find(|c| from.starts_with(c))
                    .expect("a clone from a workspace the test made");
                (
                    source.to_path_buf(),
                    from.strip_prefix(source)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .collect()
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn every_file_another_workspace_holds_is_cloned_and_the_rest_written() {
        let w = Weave::with(&[
            ("a.txt", "a\n"),
            ("dir/b.txt", "b\n"),
            ("filtered.txt", "f\n"),
            (".gitattributes", "filtered.txt filter=upper\n"),
        ]);
        let at = w.head();
        let fs = FakePrimitives::default();

        let (dest, placement) = w.populate("seat", &at, &fs);

        let cloned: Vec<String> = placements(&fs, &[&w.store])
            .into_iter()
            .map(|(source, path)| {
                assert_eq!(source, w.store);
                path
            })
            .collect();
        assert_eq!(
            sorted(cloned),
            vec![".gitattributes", "a.txt", "dir/b.txt"],
            "the filtered file has no identity and must be written by the backend"
        );
        assert_eq!(
            placement,
            FilePlacement {
                reflinked: 3,
                hardlinked: 0,
                written: 1
            }
        );
        assert_checked_out(&dest);
        assert!(
            std::fs::read_dir(&dest).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".rwv")),
            "a probe was left behind"
        );
    }

    #[test]
    fn a_filesystem_that_cannot_clone_leaves_every_file_to_the_backend() {
        let w = Weave::with(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);

        let (dest, placement) = w.populate("seat", &at, &fs);

        assert!(fs.clones().is_empty());
        assert_eq!(
            fs.attempts(),
            1,
            "a source that cannot clone costs one attempt, not one per file"
        );
        assert_eq!(
            placement,
            FilePlacement {
                reflinked: 0,
                hardlinked: 0,
                written: files_in(&dest).len()
            }
        );
        assert_checked_out(&dest);
    }

    #[test]
    fn a_source_on_another_device_is_passed_over_for_one_that_can_clone() {
        let w = Weave::with(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let at = w.head();
        let near = w.seat("near", &at);
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::CrossesDevices);

        let (dest, placement) = w.populate("seat", &at, &fs);

        let sources: BTreeSet<PathBuf> = placements(&fs, &[&w.store, &near])
            .into_iter()
            .map(|(source, _)| source)
            .collect();
        assert_eq!(sources, BTreeSet::from([near]));
        assert_eq!(placement.reflinked, 2);
        assert_checked_out(&dest);
    }

    #[test]
    fn a_source_changed_mid_copy_ends_up_checked_out_again() {
        let w = Weave::with(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.before_clone(|from, to| {
            let probe = to
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".rwv");
            if !probe && from.ends_with("a.txt") {
                std::fs::write(from, "changed under the clone\n").unwrap();
            }
        });

        let (dest, placement) = w.populate("seat", &at, &fs);

        assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "a\n");
        assert_eq!(
            placement,
            FilePlacement {
                reflinked: 1,
                hardlinked: 0,
                written: 1
            }
        );
        assert_checked_out(&dest);
    }

    #[test]
    fn a_source_file_not_holding_its_recorded_content_is_never_cloned() {
        let w = Weave::with(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let at = w.head();
        std::fs::write(w.store.join("a.txt"), "a\r\n").unwrap();
        let fs = FakePrimitives::default();

        let (dest, placement) = w.populate("seat", &at, &fs);

        let cloned: Vec<String> = placements(&fs, &[&w.store])
            .into_iter()
            .map(|(_, path)| path)
            .collect();
        assert_eq!(cloned, vec!["b.txt"]);
        assert_eq!(placement.reflinked, 1);
        assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "a\n");
        assert_checked_out(&dest);
    }

    #[test]
    fn a_workspace_at_the_target_revision_covers_the_list_before_the_primary_is_asked() {
        let w = Weave::with(&[("a.txt", "old\n"), ("b.txt", "b\n")]);
        let target = w.commit(&[("a.txt", "new\n")]);
        let at_target = w.seat("at-target", &target);
        git(&w.store, &["checkout", "-q", "--detach", "HEAD~1"]);
        let fs = FakePrimitives::default();

        let (dest, placement) = w.populate("seat", &target, &fs);

        let sources: BTreeSet<PathBuf> = placements(&fs, &[&w.store, &at_target])
            .into_iter()
            .map(|(source, _)| source)
            .collect();
        assert_eq!(sources, BTreeSet::from([at_target]));
        assert_eq!(placement.reflinked, 2);
        assert_checked_out(&dest);
    }

    #[test]
    fn an_executable_is_cloned_only_from_a_copy_with_its_executable_bit() {
        let w = Weave::with(&[("run.sh", "#!/bin/sh\n")]);
        git(&w.store, &["update-index", "--chmod=+x", "run.sh"]);
        git(&w.store, &["commit", "-q", "-m", "x"]);
        let at = w.head();
        let checked_out = w.seat("checked-out", &at);
        let fs = FakePrimitives::default();

        let (dest, placement) = w.populate("seat", &at, &fs);

        assert_eq!(
            placements(&fs, &[&w.store, &checked_out]),
            vec![(checked_out, "run.sh".to_owned())],
            "the primary's copy lacks the executable bit the tree records"
        );
        let tracked = git_vcs()
            .population()
            .unwrap()
            .tracked_files(&dest)
            .unwrap();
        assert!(matches!(
            tracked.iter().find(|f| f.path == "run.sh").unwrap().kind,
            TrackedFileKind::File {
                executable: true,
                ..
            }
        ));
        assert_eq!(placement.reflinked, 1);
        assert_checked_out(&dest);
    }

    #[test]
    fn a_source_recording_another_mode_for_the_same_content_is_not_cloned_from() {
        let w = Weave::with(&[("run.sh", "#!/bin/sh\n")]);
        let plain = w.head();
        git(&w.store, &["update-index", "--chmod=+x", "run.sh"]);
        git(&w.store, &["commit", "-q", "-m", "x"]);
        let executable = w.head();
        git(
            &w.store,
            &["checkout", "-q", "-f", "--detach", plain.as_str()],
        );
        let fs = FakePrimitives::default();

        let (dest, placement) = w.populate("seat", &executable, &fs);

        assert!(
            placements(&fs, &[&w.store]).is_empty(),
            "cloned a file the source records as not executable"
        );
        assert_eq!(placement.reflinked, 0);
        assert_checked_out(&dest);
    }

    /// Whether the filesystem under `dir` is declared able to clone; `None`
    /// where nothing is declared, and the counts are only checked to add up.
    fn declared_to_clone(dir: &Path) -> Option<bool> {
        if cfg!(target_os = "macos") {
            return Some(true);
        }
        if !cfg!(target_os = "linux") {
            return None;
        }
        let out = std::process::Command::new("stat")
            .args(["-f", "-c", "%T"])
            .arg(dir)
            .output()
            .ok()?;
        match String::from_utf8_lossy(&out.stdout).trim() {
            "ext2/ext3" | "tmpfs" => Some(false),
            "btrfs" => Some(true),
            _ => None,
        }
    }

    #[test]
    fn this_filesystem_clones_exactly_where_it_is_declared_to() {
        let w = Weave::with(&[("a.txt", "a\n"), ("dir/b.txt", "b\n")]);
        let at = w.head();

        let (dest, placement) = w.populate("seat", &at, &Filesystem);

        let tracked = files_in(&dest).len();
        match declared_to_clone(w.tmp.path()) {
            Some(true) => assert_eq!(
                placement,
                FilePlacement {
                    reflinked: tracked,
                    hardlinked: 0,
                    written: 0
                },
                "declared able to clone, on {}",
                std::env::consts::OS
            ),
            Some(false) => assert_eq!(
                placement,
                FilePlacement {
                    reflinked: 0,
                    hardlinked: 0,
                    written: tracked
                }
            ),
            None => assert_eq!(placement.reflinked + placement.written, tracked),
        }
        assert_checked_out(&dest);
    }

    #[test]
    fn a_backend_without_population_creates_the_worktree_itself() {
        let fake = crate::vcs::testing::FakeVcs::new();
        let owned = OwnedRef::from_receipt(
            PathBuf::from("/store"),
            RawRefName::new("p--w"),
            ResolvedRevisionId::from_canonical("a".repeat(40), None),
        );

        let populated = create_worktree_populated(
            &fake,
            &owned,
            Path::new("/dest"),
            &FakePrimitives::default(),
        );

        assert!(populated.created.unwrap().is_some());
        assert!(populated.filled.unwrap().is_none());
        assert_eq!(
            fake.calls(),
            vec![crate::vcs::testing::VcsCall::MaterializeWorktreeOnRef]
        );
    }

    #[test]
    fn rewritten_files_are_shared_again_and_a_local_change_is_kept() {
        let w = Weave::with(&[("a.txt", "a1\n"), ("c.txt", "c1\n"), ("b.txt", "b\n")]);
        let before = w.head();
        let after = w.commit(&[("a.txt", "a2\n"), ("c.txt", "c2\n")]);
        let member = w.seat("member", &before);
        git(&member, &["checkout", "-q", "--detach", after.as_str()]);
        std::fs::write(member.join("c.txt"), "edited here\n").unwrap();
        let changed = git_vcs()
            .changed_paths_between(&member, &before, &after)
            .unwrap();
        let fs = FakePrimitives::default();

        let placement = reshare(git_vcs().as_ref(), &member, &changed, &fs)
            .unwrap()
            .unwrap();

        let cloned: Vec<(PathBuf, String)> = placements(&fs, &[&w.store]);
        assert_eq!(cloned, vec![(w.store.clone(), "a.txt".to_owned())]);
        assert_eq!(
            std::fs::read_to_string(member.join("a.txt")).unwrap(),
            "a2\n"
        );
        assert_eq!(
            std::fs::read_to_string(member.join("c.txt")).unwrap(),
            "edited here\n",
            "re-sharing replaced a file holding a local change"
        );
        assert_eq!(
            placement,
            FilePlacement {
                reflinked: 1,
                hardlinked: 0,
                written: 1
            }
        );
    }

    #[test]
    fn nothing_is_replaced_where_nothing_can_be_cloned() {
        let w = Weave::with(&[("a.txt", "a1\n")]);
        let before = w.head();
        let after = w.commit(&[("a.txt", "a2\n")]);
        let member = w.seat("member", &before);
        git(&member, &["checkout", "-q", "--detach", after.as_str()]);
        let identity_before = file_identity(&member.join("a.txt"));
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);

        let placement = reshare(git_vcs().as_ref(), &member, &["a.txt".to_owned()], &fs)
            .unwrap()
            .unwrap();

        assert_eq!(placement.reflinked, 0);
        assert_eq!(file_identity(&member.join("a.txt")), identity_before);
    }

    #[cfg(unix)]
    fn file_identity(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).unwrap();
        (meta.dev(), meta.ino())
    }

    /// Without a stable file identity on Windows, a replacement shows as a
    /// newer modification time.
    #[cfg(windows)]
    fn file_identity(path: &Path) -> std::time::SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }
}
