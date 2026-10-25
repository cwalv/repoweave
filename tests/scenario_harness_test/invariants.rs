//! The checks every step is held to, whichever branching design is adopted.
//!
//! - **stranded**: a commit some ref of the sandbox reached before the step
//!   that no ref reaches after it, with no patch-equivalent commit reachable
//!   in its place. A reflog may still hold it; that is the case this exists to
//!   report. A project commit that changes only `rwv.lock` is derived state
//!   rwv re-creates when it relocks, and is left to the two pin checks.
//! - **unresolved pin**: an `rwv.lock` entry whose commit no ref reaches in
//!   the repository that backs that member where the lock sits. An object
//!   kept only by a reflog or awaiting gc does not count: a fresh clone would
//!   not get it.
//! - **pin rewound**: an `rwv.lock` entry whose new commit's history drops a
//!   commit the old one held, with no patch-equivalent in its place, at a step
//!   that did not name the member.
//!
//! What they read: every ref (`git rev-list --all`, which takes in each linked
//! worktree's HEAD) of every remote and every machine's canonical clone, and
//! the `rwv.lock` in the working tree of each project at each primary and live
//! workweave, plus the one committed on each project remote's `main`.
//!
//! What they cannot see: a commit that never reached a ref; a commit lost from
//! one machine while any other repository of the sandbox, a remote included,
//! still reaches it; a lock committed on any branch other than a remote's
//! `main`, or held only in the index; and content lost inside a merge commit,
//! since a merge has no patch-id and is reported whenever it is stranded or
//! dropped, which errs loud rather than blind.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::sandbox::{Place, Repo, Sandbox};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LockSite {
    /// The working-tree lock of a project at a place, keyed by the branch the
    /// project repo is on there (`DETACHED` when none), so switching branches
    /// moves between sites rather than moving a pin.
    Checkout(Place, Repo, String),
    RemoteMain(Repo),
}

pub const DETACHED: &str = "(detached)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Invariant {
    NoStrandedCommit,
    EveryPinResolves,
    NoPinRewound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    Stranded {
        repo: Repo,
        commit: String,
    },
    UnresolvedPin {
        site: LockSite,
        member: Repo,
        pin: String,
    },
    PinRewound {
        site: LockSite,
        member: Repo,
        from: String,
        to: String,
        dropped: Vec<String>,
    },
}

impl Violation {
    pub fn invariant(&self) -> Invariant {
        match self {
            Violation::Stranded { .. } => Invariant::NoStrandedCommit,
            Violation::UnresolvedPin { .. } => Invariant::EveryPinResolves,
            Violation::PinRewound { .. } => Invariant::NoPinRewound,
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::Stranded { repo, commit } => write!(
                f,
                "{}: commit {commit} was reachable from a ref and no ref reaches it or an equivalent now",
                repo.name()
            ),
            Violation::UnresolvedPin { site, member, pin } => write!(
                f,
                "{site:?}: pin {pin} for {} is not a commit in its repository",
                member.name()
            ),
            Violation::PinRewound {
                site,
                member,
                from,
                to,
                dropped,
            } => write!(
                f,
                "{site:?}: pin for {} moved {from} -> {to}, dropping {dropped:?}",
                member.name()
            ),
        }
    }
}

pub type Pins = BTreeMap<Repo, String>;

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    reachable: BTreeMap<Repo, BTreeSet<String>>,
    reachable_in_store: BTreeMap<PathBuf, BTreeSet<String>>,
    pub pins: BTreeMap<LockSite, Pins>,
}

impl Snapshot {
    pub fn reachable_from(&self, repo: Repo) -> usize {
        self.reachable.get(&repo).map_or(0, BTreeSet::len)
    }
}

pub fn snapshot(sandbox: &Sandbox) -> Snapshot {
    let mut reachable = BTreeMap::new();
    let mut reachable_in_store = BTreeMap::new();
    for repo in all_repos(sandbox) {
        let mut commits = BTreeSet::new();
        for store in sandbox.ref_stores(repo) {
            let listed: BTreeSet<String> = sandbox
                .git_ok(&store, &["rev-list", "--all"])
                .lines()
                .map(str::to_owned)
                .collect();
            commits.extend(listed.iter().cloned());
            reachable_in_store.insert(store, listed);
        }
        reachable.insert(repo, commits);
    }

    let mut pins = BTreeMap::new();
    for place in sandbox.live_places() {
        for project in [Repo::Proj, Repo::Proj2] {
            let checkout = sandbox.checkout(&place, project);
            if let Ok(text) = std::fs::read_to_string(checkout.join("rwv.lock")) {
                let out = sandbox.git(&checkout, &["symbolic-ref", "--short", "-q", "HEAD"]);
                let branch = if out.status.success() {
                    String::from_utf8(out.stdout).unwrap().trim().to_owned()
                } else {
                    DETACHED.to_owned()
                };
                pins.insert(
                    LockSite::Checkout(place.clone(), project, branch),
                    parse_pins(&text),
                );
            }
        }
    }
    for project in [Repo::Proj, Repo::Proj2] {
        let remote = sandbox.remote(project);
        if !remote.is_dir() {
            continue;
        }
        let out = sandbox.git(&remote, &["show", "refs/heads/main:rwv.lock"]);
        if out.status.success() {
            let text = String::from_utf8(out.stdout).expect("rwv.lock is UTF-8");
            pins.insert(LockSite::RemoteMain(project), parse_pins(&text));
        }
    }
    Snapshot {
        reachable,
        reachable_in_store,
        pins,
    }
}

fn all_repos(sandbox: &Sandbox) -> Vec<Repo> {
    [Repo::Proj, Repo::Proj2, Repo::App, Repo::Lib, Repo::Docs]
        .into_iter()
        .filter(|r| sandbox.remote(*r).is_dir())
        .collect()
}

fn parse_pins(text: &str) -> Pins {
    let lock = repoweave::manifest::LockFile::from_json_str(text)
        .unwrap_or_else(|e| panic!("an rwv.lock in the sandbox does not parse: {e:#}\n{text}"));
    lock.iter_entries()
        .map(|(key, entry)| {
            (
                Repo::from_member_key(&key.to_string()),
                entry.version.to_string(),
            )
        })
        .collect()
}

/// Each check, run on the state before and after one step. `may_rewind`
/// names the members the step itself declares it moves backwards. An
/// unresolved pin is charged to the step that produced it, not to every step
/// it outlives.
pub fn check(
    sandbox: &Sandbox,
    before: &Snapshot,
    after: &Snapshot,
    may_rewind: &[Repo],
) -> Vec<Violation> {
    let mut violations = stranded(sandbox, before, after);
    let already = unresolved_pins(sandbox, before);
    violations.extend(
        unresolved_pins(sandbox, after)
            .into_iter()
            .filter(|v| !already.contains(v)),
    );
    violations.extend(rewound_pins(sandbox, before, after, may_rewind));
    violations
}

pub fn stranded(sandbox: &Sandbox, before: &Snapshot, after: &Snapshot) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (repo, was) in &before.reachable {
        let empty = BTreeSet::new();
        let now = after.reachable.get(repo).unwrap_or(&empty);
        let lost: Vec<&String> = was.difference(now).collect();
        if lost.is_empty() {
            continue;
        }
        let surviving = reachable_patch_ids(sandbox, *repo);
        for commit in lost {
            if repo.is_project() && touches_only_the_lock(sandbox, *repo, commit) {
                continue;
            }
            let equivalent =
                patch_id_anywhere(sandbox, *repo, commit).is_some_and(|id| surviving.contains(&id));
            if !equivalent {
                violations.push(Violation::Stranded {
                    repo: *repo,
                    commit: commit.clone(),
                });
            }
        }
    }
    violations
}

pub fn unresolved_pins(sandbox: &Sandbox, after: &Snapshot) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (site, pins) in &after.pins {
        for (member, pin) in pins {
            let resolves = store_for(sandbox, site, *member)
                .and_then(|store| after.reachable_in_store.get(&store))
                .is_some_and(|commits| commits.contains(pin));
            if !resolves {
                violations.push(Violation::UnresolvedPin {
                    site: site.clone(),
                    member: *member,
                    pin: pin.clone(),
                });
            }
        }
    }
    violations
}

pub fn rewound_pins(
    sandbox: &Sandbox,
    before: &Snapshot,
    after: &Snapshot,
    may_rewind: &[Repo],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (site, now) in &after.pins {
        let Some(was) = before.pins.get(site) else {
            continue;
        };
        for (member, to) in now {
            let Some(from) = was.get(member) else {
                continue;
            };
            if from == to || may_rewind.contains(member) {
                continue;
            }
            let Some(store) = store_for(sandbox, site, *member) else {
                continue;
            };
            if !is_commit(sandbox, &store, from) || !is_commit(sandbox, &store, to) {
                continue;
            }
            let dropped = sandbox.git_ok(&store, &["rev-list", from, &format!("^{to}")]);
            if dropped.is_empty() {
                continue;
            }
            let gained = sandbox.git_ok(&store, &["rev-list", to, &format!("^{from}")]);
            let gained_ids: BTreeSet<String> = patch_ids(sandbox, &store, gained.lines())
                .into_values()
                .collect();
            let dropped_ids = patch_ids(sandbox, &store, dropped.lines());
            let unreplaced: Vec<String> = dropped
                .lines()
                .filter(|c| {
                    !dropped_ids
                        .get(*c)
                        .is_some_and(|id| gained_ids.contains(id))
                })
                .map(str::to_owned)
                .collect();
            if !unreplaced.is_empty() {
                violations.push(Violation::PinRewound {
                    site: site.clone(),
                    member: *member,
                    from: from.clone(),
                    to: to.clone(),
                    dropped: unreplaced,
                });
            }
        }
    }
    violations
}

pub fn store_for(sandbox: &Sandbox, site: &LockSite, member: Repo) -> Option<PathBuf> {
    let store = match site {
        LockSite::Checkout(place, _, _) => sandbox.canonical(place.machine(), member),
        LockSite::RemoteMain(_) => sandbox.remote(member),
    };
    store.is_dir().then_some(store)
}

fn is_commit(sandbox: &Sandbox, store: &Path, rev: &str) -> bool {
    sandbox
        .git(store, &["cat-file", "-e", &format!("{rev}^{{commit}}")])
        .status
        .success()
}

/// `git patch-id --stable` for each of `commits` that has one, over every
/// path but `rwv.lock`; a merge, an empty commit, or one touching only the
/// lock has none. The lock is left out because rwv rewrites it when it
/// replays a commit, so a replay's lock diff never matches the original's.
fn patch_ids<'a>(
    sandbox: &Sandbox,
    store: &Path,
    commits: impl Iterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    let commits: Vec<&str> = commits.collect();
    if commits.is_empty() {
        return BTreeMap::new();
    }
    let mut show = vec!["show", "--no-color", "--format=commit %H", "-p"];
    show.extend(commits.iter().copied());
    show.extend(["--", ".", ":(exclude)rwv.lock"]);
    let diff = sandbox.git_ok_raw(store, &show);
    sandbox
        .git_with_stdin(store, &["patch-id", "--stable"], &diff)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(id, commit)| (commit.to_owned(), id.to_owned()))
        .collect()
}

fn reachable_patch_ids(sandbox: &Sandbox, repo: Repo) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for store in sandbox.ref_stores(repo) {
        let listed = sandbox.git_ok(&store, &["rev-list", "--all", "--no-merges"]);
        ids.extend(patch_ids(sandbox, &store, listed.lines()).into_values());
    }
    ids
}

fn patch_id_anywhere(sandbox: &Sandbox, repo: Repo, commit: &str) -> Option<String> {
    sandbox
        .ref_stores(repo)
        .iter()
        .find(|store| is_commit(sandbox, store, commit))
        .and_then(|store| patch_ids(sandbox, store, std::iter::once(commit)).remove(commit))
}

fn touches_only_the_lock(sandbox: &Sandbox, repo: Repo, commit: &str) -> bool {
    sandbox
        .ref_stores(repo)
        .iter()
        .find(|store| is_commit(sandbox, store, commit))
        .is_some_and(|store| {
            sandbox.git_ok(
                store,
                &["show", "--no-renames", "--name-only", "--format=", commit],
            ) == "rwv.lock"
        })
}
