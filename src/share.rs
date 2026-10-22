//! Placing a member's tracked files from the repository's other workspaces,
//! so a workspace costs disk only where its files differ from one that
//! already holds them: by copy-on-write clone for any member, and by hard
//! link for a `reference` member's read-only files.
//!
//! The backend says what a checkout writes, which source files still hold
//! exactly that, and writes whatever was not placed; nothing here knows how a
//! VCS stores content. Nothing records how a file was placed: a later
//! operation asks the backend again.

use crate::manifest::{RepoPath, Role};
use crate::path_spelling::CanonicalPath;
use crate::vcs::{
    BornRef, OwnedRef, RecordedSubmodule, ResolvedRevisionId, SubmoduleBirth, TrackedFile, Vcs,
    VcsError, WorkspacePopulation,
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

    /// Make `to`, which must not exist, another name for `from`.
    fn hardlink(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// The filesystem this process runs on.
pub(crate) struct Filesystem;

impl FilePrimitives for Filesystem {
    fn reflink(&self, from: &Path, to: &Path) -> io::Result<()> {
        reflink_copy::reflink(from, to)
    }

    fn hardlink(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::hard_link(from, to)
    }
}

/// How a file is placed from another workspace, in the order tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Reflink,
    Hardlink,
}

/// An error saying that nothing at all can be placed between these two
/// places in a mode, as opposed to this one file failing.
fn cannot_place_between(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::CrossesDevices | io::ErrorKind::InvalidInput
    )
}

/// Whether a file placed for a member of `role` may share one file with
/// another workspace's copy: only into a `reference` member, only from one,
/// and only where the backend promises an update in one workspace leaves the
/// other's file alone.
fn may_link(role: Role, backend_declares: bool, source_is_reference: bool) -> bool {
    role == Role::Reference && backend_declares && source_is_reference
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

/// Create the workspace [`Vcs::create_worktree_on`] would, for a member of
/// `role`, with every file another workspace of the repository holds placed
/// from it and the rest written by the backend.
pub(crate) fn create_worktree_populated(
    vcs: &dyn Vcs,
    owned: &OwnedRef,
    dest: &Path,
    role: Role,
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
        Ok(_) => {
            let sources = sources(vcs, dest, owned.created_at());
            fill(population, dest, role, &sources, fs).map(Some)
        }
        Err(_) => Ok(None),
    };
    Populated { created, filled }
}

fn fill(
    population: &dyn WorkspacePopulation,
    dest: &Path,
    role: Role,
    sources: &[Source],
    fs: &dyn FilePrimitives,
) -> anyhow::Result<FilePlacement> {
    let tracked = population.tracked_files(dest)?;
    let wanted: Vec<&TrackedFile> = tracked.iter().filter(|f| f.identity().is_some()).collect();
    let placed = place(
        population,
        dest,
        role,
        sources,
        &wanted,
        fs,
        Placing::IntoEmpty,
    );
    population
        .finish_checkout(dest)
        .with_context(|| format!("failed to finish the checkout at {}", dest.display()))?;
    let mut placement = verify_and_repair(population, dest, &placed)?;
    placement.written = tracked.len() - placement.reflinked - placement.hardlinked;
    Ok(placement)
}

/// After the backend rewrote `changed` in the member of `role` checked out at
/// `member`, replace each rewritten file with one placed from another
/// workspace's copy of the same content. A file that no longer holds what the
/// member records is never replaced, so no local change is lost.
///
/// `None` for a backend that answers no population questions.
pub(crate) fn reshare(
    vcs: &dyn Vcs,
    member: &Path,
    role: Role,
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
        population,
        member,
        role,
        &sources(vcs, member, &at),
        &wanted,
        fs,
        Placing::OverIntact,
    );
    let mut placement = verify_and_repair(population, member, &placed)?;
    placement.written = rewritten.len() - placement.reflinked - placement.hardlinked;
    Ok(Some(placement))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Placing {
    /// Nothing is at the path yet.
    IntoEmpty,
    /// The path holds what the workspace records; replace it whole.
    OverIntact,
}

/// Place each of `wanted` into `dest` from a workspace holding its content,
/// trying each mode in turn, and return the paths placed with how. A file
/// nothing can be placed from is left for the backend.
fn place(
    population: &dyn WorkspacePopulation,
    dest: &Path,
    role: Role,
    sources: &[Source],
    wanted: &[&TrackedFile],
    fs: &dyn FilePrimitives,
    placing: Placing,
) -> Vec<(String, Mode)> {
    let mut uncovered: BTreeMap<&str, &TrackedFile> =
        wanted.iter().map(|f| (f.path.as_str(), *f)).collect();
    let mut placed = Vec::new();
    let backend_declares = population.updates_replace_files();
    for Source {
        path: source,
        reference: source_is_reference,
    } in sources
    {
        if uncovered.is_empty() {
            break;
        }
        let mut modes: Vec<Mode> = [Mode::Reflink, Mode::Hardlink]
            .into_iter()
            .filter(|mode| {
                *mode == Mode::Reflink || may_link(role, backend_declares, *source_is_reference)
            })
            .filter(|mode| can_place_from(source, dest, uncovered.keys().copied(), fs, *mode))
            .collect();
        if modes.is_empty() {
            continue;
        }
        let asked: Vec<String> = uncovered.keys().map(|p| (*p).to_owned()).collect();
        let Ok(held) = population.files_holding_recorded_content(source, &asked) else {
            continue;
        };
        for offered in held {
            if modes.is_empty() {
                break;
            }
            let Some(want) = uncovered.get(offered.path.as_str()).copied() else {
                continue;
            };
            if offered.kind != want.kind {
                continue;
            }
            for mode in modes.clone() {
                match place_one(source, dest, &want.path, fs, mode, placing) {
                    Ok(()) => {
                        uncovered.remove(want.path.as_str());
                        placed.push((want.path.clone(), mode));
                        break;
                    }
                    Err(e) if cannot_place_between(&e) => modes.retain(|m| *m != mode),
                    Err(_) => {}
                }
            }
        }
    }
    placed
}

/// A workspace files may be placed from.
struct Source {
    path: PathBuf,
    /// Whether its files hold the `reference` role's permissions.
    reference: bool,
}

/// Every other workspace of the repository `dest` belongs to, those at
/// `at` first and then the primary's checkout.
fn sources(vcs: &dyn Vcs, dest: &Path, at: &ResolvedRevisionId) -> Vec<Source> {
    let same = |a: &Path, b: &Path| match (CanonicalPath::existing(a), CanonicalPath::existing(b)) {
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
    found
        .into_iter()
        .map(|(_, _, path)| Source {
            reference: crate::settle::Listing::of(&path).is_reference(),
            path,
        })
        .collect()
}

/// Whether anything can be placed from `source` into `dest` in `mode`, asked
/// once by placing a file `source` has at one of `paths` and removing it, so
/// a mode the filesystem lacks costs one attempt rather than a read of every
/// source file.
fn can_place_from<'a>(
    source: &Path,
    dest: &Path,
    mut paths: impl Iterator<Item = &'a str>,
    fs: &dyn FilePrimitives,
    mode: Mode,
) -> bool {
    let Some(sample) = paths
        .find(|p| std::fs::symlink_metadata(source.join(p)).is_ok_and(|m| m.file_type().is_file()))
    else {
        return false;
    };
    let probe = dest.join(format!(".rwv-place-probe-{}", std::process::id()));
    let from = source.join(sample);
    let placed = match mode {
        Mode::Reflink => fs.reflink(&from, &probe),
        Mode::Hardlink => fs.hardlink(&from, &probe),
    }
    .is_ok();
    let _ = std::fs::remove_file(&probe);
    placed
}

/// A file that can be written in place must not gain a second name: one edit
/// would reach every workspace sharing it.
fn read_only(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_file() && !crate::settle::any_write_bit(&meta))
}

fn place_one(
    source: &Path,
    dest: &Path,
    path: &str,
    fs: &dyn FilePrimitives,
    mode: Mode,
    placing: Placing,
) -> io::Result<()> {
    let from = source.join(path);
    let to = dest.join(path);
    if mode == Mode::Hardlink && !read_only(&from) {
        return Err(io::Error::other("a writable file is never linked"));
    }
    let put = |at: &Path| -> io::Result<()> {
        match mode {
            Mode::Reflink => fs.reflink(&from, at),
            Mode::Hardlink => fs.hardlink(&from, at).and_then(|()| {
                if read_only(at) {
                    Ok(())
                } else {
                    let _ = std::fs::remove_file(at);
                    Err(io::Error::other("the linked file gained write bits"))
                }
            }),
        }
    };
    match placing {
        Placing::IntoEmpty => {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            put(&to)
        }
        Placing::OverIntact => {
            let name = to
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let staged = to.with_file_name(format!(".{name}.rwv-share"));
            let replaced = put(&staged).and_then(|()| std::fs::rename(&staged, &to));
            if replaced.is_err() {
                let _ = std::fs::remove_file(&staged);
            }
            replaced
        }
    }
}

/// Check out again every one of `placed` that does not hold exactly what the
/// workspace records, and count, by mode, those that did.
fn verify_and_repair(
    population: &dyn WorkspacePopulation,
    workspace: &Path,
    placed: &[(String, Mode)],
) -> anyhow::Result<FilePlacement> {
    let mut placement = FilePlacement::default();
    if placed.is_empty() {
        return Ok(placement);
    }
    let paths: Vec<String> = placed.iter().map(|(p, _)| p.clone()).collect();
    let holding: BTreeSet<String> = population
        .files_holding_recorded_content(workspace, &paths)?
        .into_iter()
        .map(|f| f.path)
        .collect();
    let mut mismatched = Vec::new();
    for (path, mode) in placed {
        match (holding.contains(path), mode) {
            (false, _) => mismatched.push(path.clone()),
            (true, Mode::Reflink) => placement.reflinked += 1,
            (true, Mode::Hardlink) => placement.hardlinked += 1,
        }
    }
    population
        .check_out_paths(workspace, &mismatched)
        .with_context(|| format!("failed to repair placed files in {}", workspace.display()))?;
    Ok(placement)
}

/// What [`populate_submodules`] did, one line per submodule at any depth,
/// each named by its path from the member's root.
#[derive(Debug, Default)]
pub(crate) struct SubmodulePopulation {
    /// Checked out, with how their files were placed.
    pub(crate) populated: Vec<(String, FilePlacement)>,
    /// Left uninitialized, with why.
    pub(crate) left: Vec<(String, String)>,
}

impl SubmodulePopulation {
    /// One stderr line per submodule, each opening with `prefix` and naming
    /// it under `member`.
    pub(crate) fn report(&self, prefix: &str, member: &RepoPath) {
        for (path, placement) in &self.populated {
            eprintln!("{prefix}{member}/{path}: submodule checked out ({placement})");
        }
        for (path, why) in &self.left {
            eprintln!("{prefix}warning: {member}/{path}: submodule left uninitialized: {why}");
        }
    }
}

/// Check out every submodule the member at `member` records whose directory
/// is empty, at depth, from the copies of their repositories the member's
/// canonical store already holds, with files placed from the same submodule
/// in the repository's other workspaces where they can be. Nothing is fetched:
/// a submodule whose commit is not held locally is left as it was.
pub(crate) fn populate_submodules(
    vcs: &dyn Vcs,
    member: &Path,
    role: Role,
    fs: &dyn FilePrimitives,
) -> SubmodulePopulation {
    let mut done = SubmodulePopulation::default();
    let Some(population) = vcs.population() else {
        return done;
    };
    let (Some(store), Ok(at)) = (
        vcs.resolve_canonical_store(member),
        vcs.head_revision(member),
    ) else {
        return done;
    };
    let level = Level {
        workspace: member,
        shown_as: String::new(),
        module_data: &store,
        sources: sources(vcs, member, &at),
    };
    populate_level(vcs, population, &level, role, fs, &mut done);
    done
}

/// One checkout whose submodules are being populated.
struct Level<'a> {
    workspace: &'a Path,
    /// Its path from the member's root, `/`-terminated unless it is the member.
    shown_as: String,
    module_data: &'a Path,
    /// The same checkout in the repository's other workspaces.
    sources: Vec<Source>,
}

fn populate_level(
    vcs: &dyn Vcs,
    population: &dyn WorkspacePopulation,
    level: &Level<'_>,
    role: Role,
    fs: &dyn FilePrimitives,
    done: &mut SubmodulePopulation,
) {
    let submodules = match population.recorded_submodules(level.workspace) {
        Ok(submodules) => submodules,
        Err(e) => {
            let shown = level.shown_as.trim_end_matches('/');
            let shown = if shown.is_empty() { "." } else { shown };
            done.left.push((shown.to_owned(), format!("{e:#}")));
            return;
        }
    };
    for submodule in submodules {
        let shown = format!("{}{}", level.shown_as, submodule.path);
        let dest = level.workspace.join(&submodule.path);
        if !is_empty_or_absent(&dest) {
            continue;
        }
        let source = match population.create_unpopulated_submodule(
            level.workspace,
            &submodule,
            level.module_data,
        ) {
            Ok(SubmoduleBirth::Born { source }) => source,
            Ok(SubmoduleBirth::NotLocal) => {
                done.left.push((
                    shown,
                    format!(
                        "commit {} is not in the canonical clone's copy of it",
                        submodule.commit.short()
                    ),
                ));
                continue;
            }
            Ok(SubmoduleBirth::RepositoryPresent { existing }) => {
                done.left.push((
                    shown,
                    format!(
                        "a repository for it already exists at {}",
                        existing.display()
                    ),
                ));
                continue;
            }
            Err(e) => {
                let _ = population.discard_submodule(level.workspace, &submodule);
                done.left.push((shown, format!("{e:#}")));
                continue;
            }
        };
        let nested = Level {
            workspace: &dest,
            shown_as: format!("{shown}/"),
            module_data: &source,
            sources: submodule_sources(vcs, &level.sources, &submodule),
        };
        let filled = fill(population, &dest, role, &nested.sources, fs).and_then(|placement| {
            population.connect_submodule_upstream(level.workspace, &submodule, &source)?;
            crate::settle::settle(vcs, &dest, role)?;
            Ok(placement)
        });
        match filled {
            Ok(placement) => {
                done.populated.push((shown, placement));
                populate_level(vcs, population, &nested, role, fs, done);
            }
            Err(e) => {
                let _ = population.discard_submodule(level.workspace, &submodule);
                done.left.push((shown, format!("{e:#}")));
            }
        }
    }
}

/// The checkouts of `submodule` inside `parents`, those at its recorded
/// commit first.
fn submodule_sources(
    vcs: &dyn Vcs,
    parents: &[Source],
    submodule: &RecordedSubmodule,
) -> Vec<Source> {
    let mut found: Vec<(bool, Source)> = parents
        .iter()
        .map(|parent| Source {
            path: parent.path.join(&submodule.path),
            reference: parent.reference,
        })
        .filter(|source| vcs.is_repo(&source.path))
        .map(|source| {
            let elsewhere = vcs
                .head_revision(&source.path)
                .map_or(true, |head| *head.id() != submodule.commit);
            (elsewhere, source)
        })
        .collect();
    found.sort_by_key(|(elsewhere, _)| *elsewhere);
    found.into_iter().map(|(_, source)| source).collect()
}

fn is_empty_or_absent(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! File primitives a test can fail on demand.

    use super::*;
    use std::sync::Mutex;

    type BeforePlace = Box<dyn FnMut(&Path, &Path) + Send>;

    /// Clones by copying and links for real, unless scripted to fail for a
    /// source.
    #[derive(Default)]
    pub(crate) struct FakePrimitives {
        failures: Mutex<Vec<(PathBuf, Mode, io::ErrorKind)>>,
        before_place: Mutex<Option<BeforePlace>>,
        clones: Mutex<Vec<(PathBuf, PathBuf)>>,
        links: Mutex<Vec<(PathBuf, PathBuf)>>,
        attempts: Mutex<usize>,
    }

    impl FakePrimitives {
        /// Every clone from under `source` fails with `kind`.
        pub(crate) fn fail_from(&self, source: &Path, kind: io::ErrorKind) {
            self.failures
                .lock()
                .unwrap()
                .push((source.to_path_buf(), Mode::Reflink, kind));
        }

        /// Every link from under `source` fails with `kind`.
        #[cfg(unix)]
        pub(crate) fn fail_links_from(&self, source: &Path, kind: io::ErrorKind) {
            self.failures
                .lock()
                .unwrap()
                .push((source.to_path_buf(), Mode::Hardlink, kind));
        }

        /// Run `hook` with each clone's or link's `(from, to)` just before it
        /// is made.
        pub(crate) fn before_place(&self, hook: impl FnMut(&Path, &Path) + Send + 'static) {
            *self.before_place.lock().unwrap() = Some(Box::new(hook));
        }

        /// Every clone that succeeded, as `(from, to)`.
        pub(crate) fn clones(&self) -> Vec<(PathBuf, PathBuf)> {
            self.clones.lock().unwrap().clone()
        }

        /// Every link that succeeded, as `(from, to)`.
        #[cfg(unix)]
        pub(crate) fn links(&self) -> Vec<(PathBuf, PathBuf)> {
            self.links.lock().unwrap().clone()
        }

        /// How many clones were asked for, failed ones included.
        pub(crate) fn attempts(&self) -> usize {
            *self.attempts.lock().unwrap()
        }

        fn enter(&self, from: &Path, to: &Path, mode: Mode) -> io::Result<()> {
            if let Some((_, _, kind)) =
                self.failures.lock().unwrap().iter().find(|(source, m, _)| {
                    *m == mode && CanonicalPath::of(source).contains(&CanonicalPath::of(from))
                })
            {
                return Err(io::Error::from(*kind));
            }
            if to.symlink_metadata().is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            if let Some(hook) = self.before_place.lock().unwrap().as_mut() {
                hook(from, to);
            }
            Ok(())
        }
    }

    impl FilePrimitives for FakePrimitives {
        fn reflink(&self, from: &Path, to: &Path) -> io::Result<()> {
            *self.attempts.lock().unwrap() += 1;
            self.enter(from, to, Mode::Reflink)?;
            std::fs::copy(from, to)?;
            self.clones
                .lock()
                .unwrap()
                .push((from.to_path_buf(), to.to_path_buf()));
            Ok(())
        }

        fn hardlink(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.enter(from, to, Mode::Hardlink)?;
            std::fs::hard_link(from, to)?;
            self.links
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
    use crate::git::{git_command_in_test, git_vcs, under_both_line_endings, LineEndings};
    use crate::manifest::Manifest;
    use crate::path_spelling::resolve;
    use crate::vcs::RawRefName;

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

    const REPO: &str = "github/o/r";

    /// A primary whose one project lists [`REPO`], and that repo's canonical
    /// clone, whose own checkout is the primary's.
    struct Weave {
        tmp: tempfile::TempDir,
        root: PathBuf,
        store: PathBuf,
    }

    /// What the repository declares beyond its tracked `.gitattributes`: a
    /// `*.bin` or `*.sh` file checks out as committed under either
    /// [`LineEndings`], so it can be placed from another workspace under both.
    const DECLARED_UNCONVERTED: &str = "*.bin -text\n*.sh text eol=lf\n";

    /// Of `paths`, those a checkout under `endings` writes exactly as
    /// committed, which are the ones a copy elsewhere can be placed for.
    fn unconverted(endings: LineEndings, paths: &[&str]) -> Vec<String> {
        paths
            .iter()
            .filter(|p| {
                endings == LineEndings::Verbatim || p.ends_with(".bin") || p.ends_with(".sh")
            })
            .map(|p| (*p).to_owned())
            .collect()
    }

    impl Weave {
        fn with(endings: LineEndings, files: &[(&str, &str)]) -> Self {
            Self::listing(endings, files, Role::Owned)
        }

        /// A primary whose project gives [`REPO`] the role `role`, and whose
        /// checkouts write line endings as `endings` says.
        fn listing(endings: LineEndings, files: &[(&str, &str)], role: Role) -> Self {
            let tmp = crate::tempdir().unwrap();
            // git reports worktree paths resolved, so the fakes must key on the
            // same spelling (macOS temp dirs sit behind the /var symlink).
            let root = resolve(tmp.path()).unwrap();
            let primary = root.join("primary");
            let project = crate::workspace::project_dir(
                &primary,
                &crate::manifest::ProjectName::new("app").unwrap(),
            );
            std::fs::create_dir_all(&project).unwrap();
            std::fs::write(
                project.join(Manifest::FILE_NAME),
                format!(
                    "[repositories.\"{REPO}\"]\ntype = \"git\"\n\
                     url = \"https://example.com/{REPO}.git\"\nversion = \"main\"\n\
                     role = \"{role}\"\n"
                ),
            )
            .unwrap();
            let store = primary.join(REPO);
            std::fs::create_dir_all(&store).unwrap();
            git(&store, &["init", "-q", "-b", "main"]);
            std::fs::write(store.join(".git/info/attributes"), DECLARED_UNCONVERTED).unwrap();
            endings.pin(&store);
            let w = Self { tmp, root, store };
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
            let dest = self.root.join(name);
            git(
                &self.store,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    name,
                    dest.to_str().unwrap(),
                    at.id().as_str(),
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
            self.populate_as(name, at, Role::Owned, fs)
        }

        fn populate_as(
            &self,
            name: &str,
            at: &ResolvedRevisionId,
            role: Role,
            fs: &dyn FilePrimitives,
        ) -> (PathBuf, FilePlacement) {
            let dest = self.root.join(name);
            let owned =
                OwnedRef::from_receipt(self.store.join(".git"), RawRefName::new(name), at.clone());
            let populated = create_worktree_populated(git_vcs().as_ref(), &owned, &dest, role, fs);
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

    /// What git's own checkout writes at `path` in `workspace`, conversions
    /// included.
    fn checked_out_bytes(workspace: &Path, path: &str) -> Vec<u8> {
        let out = git_command_in_test()
            .args(["cat-file", "--filters", &format!("HEAD:{path}")])
            .current_dir(workspace)
            .output()
            .unwrap();
        assert!(out.status.success(), "cat-file --filters {path}");
        out.stdout
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
        for path in files_in(dest) {
            assert_eq!(
                std::fs::read(dest.join(&path)).unwrap(),
                checked_out_bytes(dest, &path),
                "{path} is not what git's checkout writes"
            );
        }
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
                    .starts_with(".rwv-place-probe")
            })
            .map(|(from, _)| {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the source is the candidate `from` is spelled under, for the strip below"
                )]
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

    fn every_file_another_workspace_holds_is_cloned_and_the_rest_written(endings: LineEndings) {
        let w = Weave::with(
            endings,
            &[
                ("a.txt", "a\n"),
                ("dir/b.txt", "b\n"),
                ("c.bin", "c\n"),
                ("filtered.txt", "f\n"),
                (".gitattributes", "filtered.txt filter=upper\n"),
            ],
        );
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
        let expected = unconverted(endings, &[".gitattributes", "a.txt", "c.bin", "dir/b.txt"]);
        assert_eq!(
            sorted(cloned),
            expected,
            "the filtered file, and a file the checkout converts, has no identity and must \
             be written by the backend"
        );
        assert_eq!(
            placement,
            FilePlacement {
                reflinked: expected.len(),
                hardlinked: 0,
                written: 5 - expected.len()
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

    fn a_filesystem_that_cannot_clone_leaves_every_file_to_the_backend(endings: LineEndings) {
        let w = Weave::with(endings, &[("a.txt", "a\n"), ("b.bin", "b\n")]);
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

    fn a_source_whose_probe_fails_is_never_read(endings: LineEndings) {
        let w = Weave::with(
            endings,
            &[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.bin", "c\n")],
        );
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::PermissionDenied);

        let (dest, placement) = w.populate("seat", &at, &fs);

        assert_eq!(
            fs.attempts(),
            1,
            "a source whose probe fails is passed over before any of its files is tried"
        );
        assert_eq!(placement.reflinked, 0);
        assert_checked_out(&dest);
    }

    fn a_source_on_another_device_is_passed_over_for_one_that_can_clone(endings: LineEndings) {
        let w = Weave::with(endings, &[("a.txt", "a\n"), ("b.bin", "b\n")]);
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
        assert_eq!(
            placement.reflinked,
            unconverted(endings, &["a.txt", "b.bin"]).len()
        );
        assert_checked_out(&dest);
    }

    fn a_source_changed_mid_copy_ends_up_checked_out_again(endings: LineEndings) {
        let w = Weave::with(endings, &[("a.bin", "a\n"), ("b.txt", "b\n")]);
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.before_place(|from, to| {
            let probe = to
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".rwv");
            if !probe && from.ends_with("a.bin") {
                std::fs::write(from, "changed under the clone\n").unwrap();
            }
        });

        let (dest, placement) = w.populate("seat", &at, &fs);

        assert_eq!(std::fs::read_to_string(dest.join("a.bin")).unwrap(), "a\n");
        let cloned = unconverted(endings, &["b.txt"]).len();
        assert_eq!(
            placement,
            FilePlacement {
                reflinked: cloned,
                hardlinked: 0,
                written: 2 - cloned
            }
        );
        assert_checked_out(&dest);
    }

    fn a_source_file_not_holding_its_recorded_content_is_never_cloned(endings: LineEndings) {
        let w = Weave::with(
            endings,
            &[("a.bin", "a\n"), ("b.txt", "b\n"), ("c.bin", "c\n")],
        );
        let at = w.head();
        std::fs::write(w.store.join("a.bin"), "a\r\n").unwrap();
        let fs = FakePrimitives::default();

        let (dest, placement) = w.populate("seat", &at, &fs);

        let cloned: Vec<String> = placements(&fs, &[&w.store])
            .into_iter()
            .map(|(_, path)| path)
            .collect();
        let expected = unconverted(endings, &["b.txt", "c.bin"]);
        assert_eq!(sorted(cloned), expected);
        assert_eq!(placement.reflinked, expected.len());
        assert_eq!(std::fs::read_to_string(dest.join("a.bin")).unwrap(), "a\n");
        assert_checked_out(&dest);
    }

    fn a_workspace_at_the_target_revision_covers_the_list_before_the_primary_is_asked(
        endings: LineEndings,
    ) {
        let w = Weave::with(endings, &[("a.bin", "old\n"), ("b.txt", "b\n")]);
        let target = w.commit(&[("a.bin", "new\n")]);
        let at_target = w.seat("at-target", &target);
        git(&w.store, &["checkout", "-q", "--detach", "HEAD~1"]);
        let fs = FakePrimitives::default();

        let (dest, placement) = w.populate("seat", &target, &fs);

        let sources: BTreeSet<PathBuf> = placements(&fs, &[&w.store, &at_target])
            .into_iter()
            .map(|(source, _)| source)
            .collect();
        assert_eq!(sources, BTreeSet::from([at_target]));
        assert_eq!(
            placement.reflinked,
            unconverted(endings, &["a.bin", "b.txt"]).len()
        );
        assert_checked_out(&dest);
    }

    #[cfg(unix)]
    fn an_executable_is_cloned_only_from_a_copy_with_its_executable_bit(endings: LineEndings) {
        use crate::vcs::TrackedFileKind;

        let w = Weave::with(endings, &[("run.sh", "#!/bin/sh\n")]);
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

    fn a_source_recording_another_mode_for_the_same_content_is_not_cloned_from(
        endings: LineEndings,
    ) {
        let w = Weave::with(endings, &[("run.sh", "#!/bin/sh\n")]);
        let plain = w.head();
        git(&w.store, &["update-index", "--chmod=+x", "run.sh"]);
        git(&w.store, &["commit", "-q", "-m", "x"]);
        let executable = w.head();
        git(
            &w.store,
            &["checkout", "-q", "-f", "--detach", plain.id().as_str()],
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

    fn this_filesystem_clones_exactly_where_it_is_declared_to(endings: LineEndings) {
        let w = Weave::with(endings, &[("a.txt", "a\n"), ("dir/b.bin", "b\n")]);
        let at = w.head();

        let (dest, placement) = w.populate("seat", &at, &Filesystem);

        let tracked = files_in(&dest).len();
        let cloneable = unconverted(endings, &["a.txt", "dir/b.bin"]).len();
        match declared_to_clone(w.tmp.path()) {
            Some(true) => assert_eq!(
                placement,
                FilePlacement {
                    reflinked: cloneable,
                    hardlinked: 0,
                    written: tracked - cloneable
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
            ResolvedRevisionId::from_canonical(
                crate::vcs::CommitId::parse(&"a".repeat(40)).unwrap(),
                None,
            ),
        );

        let populated = create_worktree_populated(
            &fake,
            &owned,
            Path::new("/dest"),
            Role::Reference,
            &FakePrimitives::default(),
        );

        assert!(populated.created.unwrap().is_some());
        assert!(populated.filled.unwrap().is_none());
        assert_eq!(
            fake.calls(),
            vec![crate::vcs::testing::VcsCall::MaterializeWorktreeOnRef]
        );
    }

    fn rewritten_files_are_shared_again_and_a_local_change_is_kept(endings: LineEndings) {
        let w = Weave::with(
            endings,
            &[
                ("a.bin", "a1\n"),
                ("c.bin", "c1\n"),
                ("d.txt", "d1\n"),
                ("b.txt", "b\n"),
            ],
        );
        let before = w.head();
        let after = w.commit(&[("a.bin", "a2\n"), ("c.bin", "c2\n"), ("d.txt", "d2\n")]);
        let member = w.seat("member", &before);
        git(
            &member,
            &["checkout", "-q", "--detach", after.id().as_str()],
        );
        std::fs::write(member.join("c.bin"), "edited here\n").unwrap();
        let changed = git_vcs()
            .changed_paths_between(&member, &before, &after)
            .unwrap();
        let fs = FakePrimitives::default();

        let placement = reshare(git_vcs().as_ref(), &member, Role::Owned, &changed, &fs)
            .unwrap()
            .unwrap();

        let cloned: Vec<String> = placements(&fs, &[&w.store])
            .into_iter()
            .map(|(source, path)| {
                assert_eq!(source, w.store);
                path
            })
            .collect();
        let expected = unconverted(endings, &["a.bin", "d.txt"]);
        assert_eq!(sorted(cloned), expected);
        for path in ["a.bin", "d.txt"] {
            assert_eq!(
                std::fs::read(member.join(path)).unwrap(),
                checked_out_bytes(&member, path),
                "{path} is not what git's checkout writes"
            );
        }
        assert_eq!(
            std::fs::read_to_string(member.join("c.bin")).unwrap(),
            "edited here\n",
            "re-sharing replaced a file holding a local change"
        );
        assert_eq!(
            placement,
            FilePlacement {
                reflinked: expected.len(),
                hardlinked: 0,
                written: 3 - expected.len()
            }
        );
    }

    fn nothing_is_replaced_where_nothing_can_be_cloned(endings: LineEndings) {
        let w = Weave::with(endings, &[("a.bin", "a1\n")]);
        let before = w.head();
        let after = w.commit(&[("a.bin", "a2\n")]);
        let member = w.seat("member", &before);
        git(
            &member,
            &["checkout", "-q", "--detach", after.id().as_str()],
        );
        let identity_before = file_identity(&member.join("a.bin"));
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);

        let placement = reshare(
            git_vcs().as_ref(),
            &member,
            Role::Owned,
            &["a.bin".to_owned()],
            &fs,
        )
        .unwrap()
        .unwrap();

        assert_eq!(placement.reflinked, 0);
        assert_eq!(file_identity(&member.join("a.bin")), identity_before);
    }

    fn a_file_whose_clone_fails_is_left_exactly_as_it_was(endings: LineEndings) {
        let w = Weave::with(endings, &[("a.bin", "a1\n"), ("b.bin", "b1\n")]);
        let before = w.head();
        let after = w.commit(&[("a.bin", "a2\n"), ("b.bin", "b2\n")]);
        let member = w.seat("member", &before);
        git(
            &member,
            &["checkout", "-q", "--detach", after.id().as_str()],
        );
        let b_before = file_identity(&member.join("b.bin"));
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store.join("b.bin"), io::ErrorKind::PermissionDenied);
        let changed = vec!["a.bin".to_owned(), "b.bin".to_owned()];

        let placement = reshare(git_vcs().as_ref(), &member, Role::Owned, &changed, &fs)
            .unwrap()
            .unwrap();

        assert_eq!(placement.reflinked, 1);
        assert_eq!(file_identity(&member.join("b.bin")), b_before);
        assert_eq!(
            std::fs::read_to_string(member.join("b.bin")).unwrap(),
            "b2\n"
        );
        assert_eq!(git(&member, &["status", "--porcelain"]), "");
    }

    #[test]
    fn a_file_may_be_linked_only_into_and_from_a_reference_member_on_a_declared_platform() {
        for role in [Role::Owned, Role::Reference] {
            for declared in [false, true] {
                for source_is_reference in [false, true] {
                    assert_eq!(
                        may_link(role, declared, source_is_reference),
                        role == Role::Reference && declared && source_is_reference,
                        "{role} target, declared {declared}, reference source {source_is_reference}"
                    );
                }
            }
        }
    }

    /// The paths, relative to `source`, every link that placed a file came
    /// from; probes are left out.
    #[cfg(unix)]
    fn linked_from(fs: &FakePrimitives, source: &Path) -> Vec<String> {
        fs.links()
            .into_iter()
            .filter(|(_, to)| {
                !to.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".rwv-place-probe")
            })
            .map(|(from, _)| {
                from.strip_prefix(source)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    /// Whether this backend links at all on this platform.
    #[cfg(unix)]
    fn declared() -> bool {
        git_vcs().population().unwrap().updates_replace_files()
    }

    /// A primary whose reference member holds `files`, settled read-only.
    #[cfg(unix)]
    fn read_only_reference(endings: LineEndings, files: &[(&str, &str)]) -> Weave {
        let w = Weave::listing(endings, files, Role::Reference);
        crate::settle::settle(git_vcs().as_ref(), &w.store, Role::Reference).unwrap();
        w
    }

    #[cfg(unix)]
    fn mode_bits(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[cfg(unix)]
    fn a_reference_file_no_clone_can_place_is_linked_to_a_read_only_reference_copy(
        endings: LineEndings,
    ) {
        let w = read_only_reference(endings, &[("a.txt", "a\n"), ("dir/b.bin", "b\n")]);
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);

        let (dest, placement) = w.populate_as("seat", &at, Role::Reference, &fs);

        let tracked = files_in(&dest).len();
        let linkable = unconverted(endings, &["a.txt", "dir/b.bin"]);
        if declared() {
            assert_eq!(
                placement,
                FilePlacement {
                    reflinked: 0,
                    hardlinked: linkable.len(),
                    written: tracked - linkable.len()
                }
            );
            for path in linkable {
                assert_eq!(
                    file_identity(&dest.join(&path)),
                    file_identity(&w.store.join(&path)),
                    "{path} does not share the source's file"
                );
                assert_eq!(
                    mode_bits(&dest.join(&path)) & 0o222,
                    0,
                    "{path} is writable"
                );
            }
        } else {
            assert_eq!(
                placement.hardlinked,
                0,
                "linked on {}",
                std::env::consts::OS
            );
            assert!(linked_from(&fs, &w.store).is_empty());
        }
        assert_checked_out(&dest);
    }

    #[cfg(unix)]
    fn a_writable_source_file_is_never_linked(endings: LineEndings) {
        let w = read_only_reference(endings, &[("a.bin", "a\n"), ("b.bin", "b\n")]);
        let at = w.head();
        crate::settle::settle(git_vcs().as_ref(), &w.store, Role::Owned).unwrap();
        let read_only = w.store.join("b.bin");
        let meta = std::fs::metadata(&read_only).unwrap();
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&read_only, perms).unwrap();
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);

        let (dest, placement) = w.populate_as("seat", &at, Role::Reference, &fs);

        let linked = linked_from(&fs, &w.store);
        assert!(
            !linked.contains(&"a.bin".to_owned()),
            "linked a writable file: {linked:?}"
        );
        if declared() {
            assert_eq!(linked, vec!["b.bin".to_owned()]);
            assert_eq!(placement.hardlinked, 1);
        }
        assert_checked_out(&dest);
    }

    #[cfg(unix)]
    fn a_member_that_is_not_reference_never_shares_a_file(endings: LineEndings) {
        let w = read_only_reference(endings, &[("a.bin", "a\n")]);
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);

        let (dest, placement) = w.populate_as("seat", &at, Role::Owned, &fs);

        assert!(
            linked_from(&fs, &w.store).is_empty(),
            "linked into an owned member"
        );
        assert_eq!(placement.hardlinked, 0);
        assert_ne!(
            file_identity(&dest.join("a.bin")),
            file_identity(&w.store.join("a.bin"))
        );
        assert_checked_out(&dest);
    }

    #[cfg(unix)]
    fn a_source_not_checked_out_as_reference_is_never_linked_from(endings: LineEndings) {
        let w = Weave::listing(endings, &[("a.bin", "a\n")], Role::Owned);
        crate::settle::settle(git_vcs().as_ref(), &w.store, Role::Reference).unwrap();
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);

        let (dest, placement) = w.populate_as("seat", &at, Role::Reference, &fs);

        assert!(
            linked_from(&fs, &w.store).is_empty(),
            "linked from a member its own project calls owned"
        );
        assert_eq!(placement.hardlinked, 0);
        assert_checked_out(&dest);
    }

    #[cfg(unix)]
    fn a_clone_is_preferred_to_a_link(endings: LineEndings) {
        let w = read_only_reference(endings, &[("a.bin", "a\n")]);
        let at = w.head();
        let fs = FakePrimitives::default();

        let (dest, placement) = w.populate_as("seat", &at, Role::Reference, &fs);

        assert_eq!(placement.reflinked, 1);
        assert_eq!(placement.hardlinked, 0);
        assert_ne!(
            file_identity(&dest.join("a.bin")),
            file_identity(&w.store.join("a.bin"))
        );
        assert_checked_out(&dest);
    }

    #[cfg(unix)]
    fn a_link_across_devices_leaves_the_file_to_the_backend(endings: LineEndings) {
        let w = read_only_reference(endings, &[("a.txt", "a\n"), ("b.bin", "b\n")]);
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);
        fs.fail_links_from(&w.store, io::ErrorKind::CrossesDevices);

        let (dest, placement) = w.populate_as("seat", &at, Role::Reference, &fs);

        assert_eq!(placement.hardlinked, 0);
        assert_eq!(placement.written, files_in(&dest).len());
        assert_checked_out(&dest);
    }

    #[cfg(unix)]
    fn a_source_made_writable_as_it_is_linked_is_not_left_sharing(endings: LineEndings) {
        use std::os::unix::fs::PermissionsExt;
        let w = read_only_reference(endings, &[("a.bin", "a\n")]);
        let at = w.head();
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);
        fs.before_place(|from, to| {
            let probe = to
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".rwv");
            if !probe {
                std::fs::set_permissions(from, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
        });

        let (dest, placement) = w.populate_as("seat", &at, Role::Reference, &fs);

        assert_eq!(placement.hardlinked, 0);
        assert_ne!(
            file_identity(&dest.join("a.bin")),
            file_identity(&w.store.join("a.bin")),
            "a file that gained write bits was left with two names"
        );
        assert_checked_out(&dest);
    }

    #[cfg(unix)]
    fn a_reference_member_moved_shares_its_rewritten_files_by_link(endings: LineEndings) {
        let w = Weave::listing(
            endings,
            &[("a.bin", "a1\n"), ("b.txt", "b\n")],
            Role::Reference,
        );
        let before = w.head();
        let after = w.commit(&[("a.bin", "a2\n")]);
        crate::settle::settle(git_vcs().as_ref(), &w.store, Role::Reference).unwrap();
        let member = w.seat("member", &before);
        git(
            &member,
            &["checkout", "-q", "--detach", after.id().as_str()],
        );
        let fs = FakePrimitives::default();
        fs.fail_from(&w.store, io::ErrorKind::Unsupported);

        let placement = reshare(
            git_vcs().as_ref(),
            &member,
            Role::Reference,
            &["a.bin".to_owned()],
            &fs,
        )
        .unwrap()
        .unwrap();

        if declared() {
            assert_eq!(placement.hardlinked, 1);
            assert_eq!(
                file_identity(&member.join("a.bin")),
                file_identity(&w.store.join("a.bin"))
            );
        } else {
            assert_eq!(placement.hardlinked, 0);
        }
        assert_eq!(
            std::fs::read_to_string(member.join("a.bin")).unwrap(),
            "a2\n"
        );
        assert_eq!(git(&member, &["status", "--porcelain"]), "");
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

    under_both_line_endings!(
        every_file_another_workspace_holds_is_cloned_and_the_rest_written,
        a_filesystem_that_cannot_clone_leaves_every_file_to_the_backend,
        a_source_whose_probe_fails_is_never_read,
        a_source_on_another_device_is_passed_over_for_one_that_can_clone,
        a_source_changed_mid_copy_ends_up_checked_out_again,
        a_source_file_not_holding_its_recorded_content_is_never_cloned,
        a_workspace_at_the_target_revision_covers_the_list_before_the_primary_is_asked,
        #[cfg(unix)]
        an_executable_is_cloned_only_from_a_copy_with_its_executable_bit,
        a_source_recording_another_mode_for_the_same_content_is_not_cloned_from,
        this_filesystem_clones_exactly_where_it_is_declared_to,
        rewritten_files_are_shared_again_and_a_local_change_is_kept,
        nothing_is_replaced_where_nothing_can_be_cloned,
        a_file_whose_clone_fails_is_left_exactly_as_it_was,
        #[cfg(unix)]
        a_reference_file_no_clone_can_place_is_linked_to_a_read_only_reference_copy,
        #[cfg(unix)]
        a_writable_source_file_is_never_linked,
        #[cfg(unix)]
        a_member_that_is_not_reference_never_shares_a_file,
        #[cfg(unix)]
        a_source_not_checked_out_as_reference_is_never_linked_from,
        #[cfg(unix)]
        a_clone_is_preferred_to_a_link,
        #[cfg(unix)]
        a_link_across_devices_leaves_the_file_to_the_backend,
        #[cfg(unix)]
        a_source_made_writable_as_it_is_linked_is_not_left_sharing,
        #[cfg(unix)]
        a_reference_member_moved_shares_its_rewritten_files_by_link,
    );
}
