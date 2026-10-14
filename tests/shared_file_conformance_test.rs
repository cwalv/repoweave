//! Whether git leaves a file alone in one workspace while another workspace
//! that shares it updates.
//!
//! The property: an update in one workspace changes neither the content nor
//! the permissions of a file in another. A backend may declare
//! `WorkspacePopulation::updates_replace_files` on a platform only where the
//! tests here hold there. Where the property does not hold, a test asserts
//! what the platform does instead, so a change in git turns it red rather
//! than passing unnoticed.
//!
//! Each update is the `Vcs` method rwv's verbs reach for it, called directly:
//! the witness-carrying wrappers rwv calls delegate to these with the same
//! arguments, and a test crate cannot mint their consent tokens.

use repoweave::git::{git_vcs, restore_paths_from_head};
use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::vcs::{
    ContentIdentity, DerivedContentPolicy, EphemeralRefName, RawRefName, ResolvedRevisionId,
    TrackedFile, TrackingRef, Vcs, WorkspacePopulation,
};
use repoweave::workweave_index::RefRegistry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

mod common;

const SHARED: &str = "shared.txt";
const BEFORE: &[u8] = b"before\n";
const AFTER: &[u8] = b"after\n";

/// A primary holding one project, and a member store whose `main` has two
/// commits: `before` and then `after`, which differ in [`SHARED`].
struct Weave {
    tmp: TempDir,
    primary: PathBuf,
    store: PathBuf,
    before: ResolvedRevisionId,
    after: ResolvedRevisionId,
}

impl Weave {
    fn new() -> Self {
        Self::with_files(
            &[(SHARED, BEFORE), ("other.txt", b"other\n")],
            &[(SHARED, AFTER)],
        )
    }

    fn with_files(first: &[(&str, &[u8])], second: &[(&str, &[u8])]) -> Self {
        let tmp = common::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        std::fs::create_dir_all(primary.join("projects/conformance")).unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        common::git_in(&store, &["init", "--initial-branch=main"]);
        common::git_in(&store, &["config", "core.autocrlf", "false"]);
        let before = commit(&store, first);
        let after = commit(&store, second);
        let store = store.canonicalize().unwrap();
        Self {
            tmp,
            primary,
            store,
            before,
            after,
        }
    }

    /// A workspace git checked out itself, on a branch of its own.
    fn checked_out(&self, name: &str, at: &ResolvedRevisionId) -> PathBuf {
        let dest = self.tmp.path().join(name);
        common::git_in(
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

    /// A workspace with nothing written, created through the backend from a
    /// receipt the registry recorded.
    fn unpopulated(&self, vcs: &dyn Vcs, name: &str, at: &ResolvedRevisionId) -> PathBuf {
        let project = ProjectName::new("conformance").unwrap();
        let receipt = RefRegistry::for_project(&self.primary, &project)
            .record_created(
                &self.store,
                EphemeralRefName::mint(&project, &WorkweaveName::new(name).unwrap()),
                at.clone(),
            )
            .unwrap();
        let dest = self.tmp.path().join(name);
        let born = population(vcs)
            .create_unpopulated_worktree_on(&receipt, &dest)
            .unwrap();
        assert!(born.is_some(), "fixture check: the ref is new");
        dest
    }
}

fn commit(repo: &Path, files: &[(&str, &[u8])]) -> ResolvedRevisionId {
    for (name, bytes) in files {
        std::fs::write(repo.join(name), bytes).unwrap();
    }
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-q", "-m", "commit"]);
    git_vcs().head_revision(repo).unwrap()
}

fn population(vcs: &dyn Vcs) -> &dyn WorkspacePopulation {
    vcs.population()
        .expect("the git under test answers the population questions")
}

fn set_writable(path: &Path, writable: bool) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions.mode();
        permissions.set_mode(if writable {
            mode | 0o200
        } else {
            mode & !0o222
        });
    }
    #[cfg(windows)]
    permissions.set_readonly(!writable);
    std::fs::set_permissions(path, permissions).unwrap();
}

/// Replace `at` with a hard link to `source`.
fn share(source: &Path, at: &Path) {
    std::fs::remove_file(at).unwrap();
    std::fs::hard_link(source, at).unwrap();
}

/// Everything about a file another workspace's update must leave alone.
#[derive(Debug, PartialEq, Eq)]
struct Kept {
    content: Vec<u8>,
    #[cfg(unix)]
    inode: (u64, u64),
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    links: u64,
    #[cfg(windows)]
    read_only: bool,
}

fn kept(path: &Path) -> Kept {
    let meta = std::fs::metadata(path).unwrap();
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Kept {
        content: std::fs::read(path).unwrap(),
        #[cfg(unix)]
        inode: (meta.dev(), meta.ino()),
        #[cfg(unix)]
        mode: meta.mode() & 0o7777,
        #[cfg(unix)]
        links: meta.nlink(),
        #[cfg(windows)]
        read_only: meta.permissions().readonly(),
    }
}

// ---------------------------------------------------------------------------
// 1 and 3. Replace, never rewrite
// ---------------------------------------------------------------------------

/// An update rwv runs on a member's files.
#[derive(Debug, Clone, Copy)]
enum Update {
    DetachedCheckout,
    BranchCheckout,
    HardReset,
    FastForward,
    RebaseWithNothingToReplay,
    PathCheckout,
    WorkingTreeRefresh,
    Repair,
}

impl Update {
    /// A tree-level update moves the workspace from `before` to `after`. A
    /// path-level one finds it already at `after`, with the shared file
    /// still holding `before`.
    fn starts_after(self) -> bool {
        matches!(
            self,
            Update::PathCheckout | Update::WorkingTreeRefresh | Update::Repair
        )
    }

    fn run(self, w: &Weave, vcs: &dyn Vcs, updater: &Path) {
        let paths = vec![SHARED.to_owned()];
        match self {
            Update::DetachedCheckout => vcs.set_detached_head(updater, &w.after).unwrap(),
            Update::BranchCheckout => {
                common::git_in(&w.store, &["branch", "at-after", w.after.as_str()]);
                let at_after = TrackingRef::parse(RawRefName::new("at-after"))
                    .unwrap()
                    .local_counterpart();
                vcs.attach_head_to(updater, &at_after).unwrap();
            }
            Update::HardReset => vcs.hard_reset(updater, &w.after).unwrap(),
            Update::FastForward => vcs.advance_if_fast_forward(updater, &w.after).unwrap(),
            Update::RebaseWithNothingToReplay => vcs
                .rebase(
                    updater,
                    &w.after,
                    &w.before,
                    DerivedContentPolicy::vcs_default(),
                )
                .unwrap(),
            Update::PathCheckout => restore_paths_from_head(updater, &paths).unwrap(),
            Update::WorkingTreeRefresh => vcs.refresh_working_tree_to_head_if_safe(updater),
            Update::Repair => population(vcs).check_out_paths(updater, &paths).unwrap(),
        }
    }
}

/// Two workspaces share [`SHARED`]; one of them updates it. Returns what the
/// other workspace's file looked like before and after.
fn update_one_of_two_sharing(update: Update, writable: bool) -> (Kept, Kept) {
    let w = Weave::new();
    let vcs = git_vcs();
    let keeper = w.checked_out("keeper", &w.before);
    let start = if update.starts_after() {
        &w.after
    } else {
        &w.before
    };
    let updater = w.checked_out("updater", start);
    set_writable(&keeper.join(SHARED), writable);
    share(&keeper.join(SHARED), &updater.join(SHARED));
    let before = kept(&keeper.join(SHARED));

    update.run(&w, vcs.as_ref(), &updater);

    assert_eq!(
        std::fs::read(updater.join(SHARED)).unwrap(),
        AFTER,
        "{update:?} did not update the file, so this tests nothing"
    );
    (before, kept(&keeper.join(SHARED)))
}

/// git replaces a file by unlinking it and creating another, so the other
/// link keeps the inode, its content and its mode, and loses one link.
#[cfg(unix)]
fn assert_left_alone(update: Update, before: Kept, after: Kept) {
    assert_eq!(
        after,
        Kept {
            links: before.links - 1,
            ..before
        },
        "{update:?} in one workspace changed the file another workspace shares, on {}",
        std::env::consts::OS
    );
}

/// The property does not hold here, and this asserts how it fails. Git for
/// Windows clears the read-only attribute on a file before deleting it, and
/// NTFS keeps that attribute on the file every link shares, so the other
/// workspace's copy keeps its content and loses its read-only attribute.
#[cfg(windows)]
fn assert_left_alone(update: Update, before: Kept, after: Kept) {
    assert_eq!(
        after,
        Kept {
            read_only: false,
            ..before
        },
        "{update:?}: Git for Windows does not behave as this test expects; re-derive \
         the hardlink declaration for Windows"
    );
}

macro_rules! replace_never_rewrite {
    ($($name:ident: $update:expr, writable = $writable:expr;)*) => {$(
        #[test]
        fn $name() {
            let (before, after) = update_one_of_two_sharing($update, $writable);
            assert_left_alone($update, before, after);
        }
    )*};
}

replace_never_rewrite! {
    a_detached_checkout_replaces_a_shared_file: Update::DetachedCheckout, writable = true;
    a_detached_checkout_replaces_a_shared_file_without_write_bits: Update::DetachedCheckout, writable = false;
    a_branch_checkout_replaces_a_shared_file: Update::BranchCheckout, writable = true;
    a_branch_checkout_replaces_a_shared_file_without_write_bits: Update::BranchCheckout, writable = false;
    a_hard_reset_replaces_a_shared_file: Update::HardReset, writable = true;
    a_hard_reset_replaces_a_shared_file_without_write_bits: Update::HardReset, writable = false;
    a_fast_forward_replaces_a_shared_file: Update::FastForward, writable = true;
    a_fast_forward_replaces_a_shared_file_without_write_bits: Update::FastForward, writable = false;
    a_rebase_with_nothing_to_replay_replaces_a_shared_file: Update::RebaseWithNothingToReplay, writable = true;
    a_rebase_with_nothing_to_replay_replaces_a_shared_file_without_write_bits: Update::RebaseWithNothingToReplay, writable = false;
    a_path_checkout_replaces_a_shared_file: Update::PathCheckout, writable = true;
    a_path_checkout_replaces_a_shared_file_without_write_bits: Update::PathCheckout, writable = false;
    a_working_tree_refresh_replaces_a_shared_file: Update::WorkingTreeRefresh, writable = true;
    a_working_tree_refresh_replaces_a_shared_file_without_write_bits: Update::WorkingTreeRefresh, writable = false;
    a_repair_replaces_a_shared_file: Update::Repair, writable = true;
    a_repair_replaces_a_shared_file_without_write_bits: Update::Repair, writable = false;
}

/// The backend declares the property exactly where the tests above observe it.
/// A platform joins by its CI leg passing them and this pin changing with it.
#[test]
fn the_declaration_follows_where_the_property_is_observed() {
    let declared = population(git_vcs().as_ref()).updates_replace_files();
    assert_eq!(
        declared,
        cfg!(target_os = "linux"),
        "on {}",
        std::env::consts::OS
    );
}

// ---------------------------------------------------------------------------
// 2. Population leaves placed files alone
// ---------------------------------------------------------------------------

#[test]
fn population_leaves_every_placed_file_as_it_was_placed() {
    let w = Weave::with_files(
        &[
            ("linked.txt", b"linked\n"),
            ("read-only.txt", b"read-only\n"),
            ("copied.txt", b"copied\n"),
            ("run.sh", b"#!/bin/sh\n"),
        ],
        &[("unplaced.txt", b"unplaced\n")],
    );
    common::git_in(&w.store, &["update-index", "--chmod=+x", "run.sh"]);
    common::git_in(&w.store, &["commit", "-q", "-m", "executable"]);
    let vcs = git_vcs();
    let at = vcs.head_revision(&w.store).unwrap();
    let source = w.checked_out("source", &at);
    set_writable(&source.join("read-only.txt"), false);
    let target = w.unpopulated(vcs.as_ref(), "target", &at);

    let linked = ["linked.txt", "read-only.txt", "run.sh"];
    let copied = ["copied.txt"];
    let mut placed: Vec<String> = linked
        .iter()
        .chain(&copied)
        .map(|p| (*p).to_owned())
        .collect();
    placed.sort();
    let wanted = identities(population(vcs.as_ref()).tracked_files(&target).unwrap());
    let offered = identities(
        population(vcs.as_ref())
            .files_holding_recorded_content(&source, &placed)
            .unwrap(),
    );
    for path in &placed {
        assert!(
            wanted[path].is_some() && wanted[path] == offered[path],
            "fixture check: {path} is not shareable from the source"
        );
    }
    for path in linked {
        std::fs::hard_link(source.join(path), target.join(path)).unwrap();
    }
    for path in copied {
        std::fs::copy(source.join(path), target.join(path)).unwrap();
    }
    let as_placed: Vec<Kept> = placed.iter().map(|p| kept(&target.join(p))).collect();

    population(vcs.as_ref()).finish_checkout(&target).unwrap();

    let as_finished: Vec<Kept> = placed.iter().map(|p| kept(&target.join(p))).collect();
    assert_eq!(
        as_finished,
        as_placed,
        "finishing touched a placed file, on {}",
        std::env::consts::OS
    );
    let holding: Vec<String> = population(vcs.as_ref())
        .files_holding_recorded_content(&target, &placed)
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(holding, placed);
    assert_eq!(
        std::fs::read(target.join("unplaced.txt")).unwrap(),
        b"unplaced\n",
        "finishing did not write what was not placed"
    );
    assert_still_shared(&source, &target, &linked);
}

#[cfg(unix)]
fn assert_still_shared(source: &Path, target: &Path, linked: &[&str]) {
    for path in linked {
        assert_eq!(
            kept(&source.join(path)).inode,
            kept(&target.join(path)).inode,
            "{path} no longer shares its source's file, on {}",
            std::env::consts::OS
        );
    }
}

/// A stable `std` reads no file identity on Windows, so sharing shows as a
/// write to the source reaching the target.
#[cfg(windows)]
fn assert_still_shared(source: &Path, target: &Path, linked: &[&str]) {
    for path in linked {
        set_writable(&source.join(path), true);
        std::fs::write(source.join(path), b"written through the source\n").unwrap();
        assert_eq!(
            std::fs::read(target.join(path)).unwrap(),
            b"written through the source\n",
            "{path} no longer shares its source's file"
        );
    }
}

fn identities(files: Vec<TrackedFile>) -> BTreeMap<String, Option<ContentIdentity>> {
    files
        .into_iter()
        .map(|f| {
            let identity = f.identity().cloned();
            (f.path, identity)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 4. Conversions
// ---------------------------------------------------------------------------

/// Equal identities check out to equal bytes, across workspaces whose
/// `core.autocrlf` differs, over every conversion attribute; a path with an
/// external filter has no identity.
#[test]
fn equal_identities_check_out_to_equal_bytes() {
    const SAME: &[u8] = "one\ntwo $Id$ caf\u{e9}\n".as_bytes();
    const ATTRIBUTES: &[(&str, &str)] = &[
        ("plain.txt", ""),
        ("text.txt", "text"),
        ("auto.txt", "text=auto"),
        ("lf.txt", "eol=lf"),
        ("crlf.txt", "eol=crlf"),
        ("ident.txt", "ident"),
        ("encoded.txt", "working-tree-encoding=ISO-8859-1"),
        ("filtered.txt", "filter=upper"),
    ];
    let content: Vec<(&str, &[u8])> = ATTRIBUTES.iter().map(|(p, _)| (*p, SAME)).collect();
    let gitattributes: String = ATTRIBUTES
        .iter()
        .filter(|(_, a)| !a.is_empty())
        .map(|(p, a)| format!("{p} {a}\n"))
        .collect();
    let w = Weave::with_files(&content, &[(".gitattributes", gitattributes.as_bytes())]);
    common::git_in(&w.store, &["config", "extensions.worktreeConfig", "true"]);
    common::git_in(&w.store, &["config", "filter.upper.smudge", "tr a-z A-Z"]);
    common::git_in(&w.store, &["config", "filter.upper.clean", "cat"]);
    let vcs = git_vcs();

    let mut checked_out: Vec<CheckedOut> = Vec::new();
    for (name, autocrlf) in [
        ("false-a", "false"),
        ("false-b", "false"),
        ("true-a", "true"),
        ("true-b", "true"),
    ] {
        let workspace = w.unpopulated(vcs.as_ref(), name, &w.after);
        common::git_in(
            &workspace,
            &["config", "--worktree", "core.autocrlf", autocrlf],
        );
        let tracked = population(vcs.as_ref()).tracked_files(&workspace).unwrap();
        population(vcs.as_ref())
            .finish_checkout(&workspace)
            .unwrap();
        for file in tracked {
            checked_out.push(CheckedOut {
                workspace: name,
                bytes: std::fs::read(workspace.join(&file.path)).unwrap(),
                identity: file.identity().cloned(),
                path: file.path,
            });
        }
    }

    let mut by_identity: BTreeMap<&ContentIdentity, Vec<&CheckedOut>> = BTreeMap::new();
    for file in &checked_out {
        if let Some(identity) = &file.identity {
            by_identity.entry(identity).or_default().push(file);
        }
    }
    for holders in by_identity.values() {
        for other in &holders[1..] {
            assert_eq!(
                other.bytes, holders[0].bytes,
                "{}/{} and {}/{} share an identity but check out to different bytes",
                other.workspace, other.path, holders[0].workspace, holders[0].path
            );
        }
    }
    let filtered: Vec<&CheckedOut> = checked_out
        .iter()
        .filter(|f| f.path == "filtered.txt")
        .collect();
    assert!(filtered.iter().all(|f| f.identity.is_none()));
    assert!(
        filtered.iter().all(|f| f.bytes != SAME),
        "fixture check: the filter driver did not run, so this tests nothing"
    );
    assert!(
        by_identity.values().any(|holders| holders
            .iter()
            .map(|f| f.workspace)
            .collect::<BTreeSet<_>>()
            .len()
            > 1),
        "no identity was shared across workspaces, so equality was never tested"
    );
    let distinct_bytes: BTreeSet<&[u8]> = checked_out
        .iter()
        .filter(|f| f.path != ".gitattributes")
        .map(|f| f.bytes.as_slice())
        .collect();
    assert!(
        distinct_bytes.len() > 2,
        "one blob checked out to too few forms for any conversion to have run"
    );
}

struct CheckedOut {
    workspace: &'static str,
    path: String,
    identity: Option<ContentIdentity>,
    bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// 5. ctime
// ---------------------------------------------------------------------------

/// Linking a file, and taking its write bits, change its ctime; git's stat
/// check reads ctime, so every workspace sharing the file must hash it again.
///
/// Unix only: Git for Windows reads a file's creation time as its ctime,
/// which neither changes.
#[cfg(unix)]
#[test]
fn sharing_or_settling_a_file_makes_its_other_workspaces_hash_it_again() {
    let w = Weave::new();
    let source = w.checked_out("source", &w.after);
    stat_clean_and_settled(&source);
    let stale = |trust_ctime: &str| {
        common::git_in(
            &source,
            &[
                "-c",
                &format!("core.trustctime={trust_ctime}"),
                "diff-files",
                "--name-only",
            ],
        )
    };
    assert_eq!(
        stale("true"),
        "",
        "fixture check: the source starts stat-clean"
    );

    let elsewhere = w.tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    wait_for_the_ctime_second_to_turn(&w.tmp.path().join("probe"));
    std::fs::hard_link(source.join(SHARED), elsewhere.join(SHARED)).unwrap();
    assert_eq!(stale("true"), SHARED, "linking left the ctime unchanged");
    assert_eq!(stale("false"), "");

    stat_clean_and_settled(&source);
    wait_for_the_ctime_second_to_turn(&w.tmp.path().join("probe"));
    set_writable(&source.join("other.txt"), false);
    assert_eq!(
        stale("true"),
        "other.txt",
        "removing write bits left the ctime unchanged"
    );
    assert_eq!(stale("false"), "");
}

/// Back-date every tracked file and refresh, so no entry is racily clean and
/// the index's stat data is the only thing a later check compares.
#[cfg(unix)]
fn stat_clean_and_settled(workspace: &Path) {
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    for path in common::git_in(workspace, &["ls-files"]).lines() {
        std::fs::File::options()
            .write(true)
            .open(workspace.join(path))
            .or_else(|_| std::fs::File::open(workspace.join(path)))
            .unwrap()
            .set_modified(past)
            .unwrap();
    }
    common::git_in(workspace, &["update-index", "-q", "--refresh"]);
}

/// Return once a file changed now gets a ctime in a later second than every
/// file changed before the call. git compares ctime to the second unless it
/// was built to read nanoseconds, and the filesystem stamps ctime from a
/// coarse clock that can trail the system clock, so the system clock cannot
/// say when the second has turned.
#[cfg(unix)]
fn wait_for_the_ctime_second_to_turn(probe: &Path) {
    use std::os::unix::fs::MetadataExt;
    let stamp = || {
        std::fs::write(probe, b"").unwrap();
        std::fs::metadata(probe).unwrap().ctime()
    };
    let started = stamp();
    while stamp() == started {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The cost behind the test above, on a repo large enough to matter. A
/// measurement, not a check: run it by name with `--ignored --nocapture`.
#[cfg(unix)]
#[test]
#[ignore = "measurement; run explicitly with --ignored --nocapture"]
fn measure_status_after_a_shared_files_ctime_moves() {
    const FILES: usize = 20_000;
    const BYTES_PER_FILE: usize = 10 * 1024;
    let w = Weave::new();
    for i in 0..FILES {
        let dir = w.store.join(format!("d{:03}", i % 200));
        std::fs::create_dir_all(&dir).unwrap();
        let body: Vec<u8> = format!("{i:08}\n")
            .bytes()
            .cycle()
            .take(BYTES_PER_FILE)
            .collect();
        std::fs::write(dir.join(format!("f{i:05}.txt")), body).unwrap();
    }
    common::git_in(&w.store, &["add", "."]);
    common::git_in(&w.store, &["commit", "-q", "-m", "bulk"]);
    let at = git_vcs().head_revision(&w.store).unwrap();
    let source = w.checked_out("source", &at);
    stat_clean_and_settled(&source);
    let tracked: Vec<String> = common::git_in(&source, &["ls-files"])
        .lines()
        .map(str::to_owned)
        .collect();

    let status = |trust_ctime: &str| {
        let started = std::time::Instant::now();
        common::git_in(
            &source,
            &[
                "-c",
                &format!("core.trustctime={trust_ctime}"),
                "status",
                "--porcelain",
                "--untracked-files=no",
            ],
        );
        started.elapsed()
    };
    let link_everything_into = |name: &str| {
        let started = std::time::Instant::now();
        for path in &tracked {
            let to = w.tmp.path().join(name).join(path);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::hard_link(source.join(path), to).unwrap();
        }
        started.elapsed()
    };

    println!(
        "{} tracked files, {} MiB",
        tracked.len(),
        tracked.len() * BYTES_PER_FILE / (1024 * 1024)
    );
    println!("status, stat-clean, trustctime=true: {:?}", status("true"));
    for trial in 1..=3 {
        wait_for_the_ctime_second_to_turn(&w.tmp.path().join("probe"));
        let linking = link_everything_into(&format!("seat{trial}"));
        let ignoring = status("false");
        let rehashing = status("true");
        let after = status("true");
        println!(
            "trial {trial}: linking {linking:?}; then status with trustctime=false {ignoring:?}, \
             trustctime=true {rehashing:?}, again {after:?}"
        );
    }
}
