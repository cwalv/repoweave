//! Git implementation of the [`Vcs`] trait.

use crate::op_state::SavepointKey;
use crate::path_spelling::CanonicalPath;
use crate::registry::{CloneUrl, RecordedRemote};
use crate::vcs::{
    AbsentRemote, Annotation, AttachedRef, Backup, BackupMark, BackupName, BackupSource,
    BackupWitness, BranchChange, BranchTip, CommitId, CommitSummary, CommittedFile, ConflictOp,
    ContentIdentity, Copied, DerivedContentPolicy, DerivedContentResolution, DestroyProof,
    DiffBeyond, DiscardOutcome, FileChange, ForeignTipPolicy, HeadAttachment, HeadObservation,
    Holds, ListedBackup, LocalRefName, LockDriverPart, LockDriverSetup, MemberBirth, MidOperation,
    NamedBranch, PatchEquivalents, PathStanding, PinOutcome, PruningFetch, PublishRef, RangeCommit,
    RangeListing, RawRefName, RawRevisionId, RecordedSubmodule, RecordedUrl, RecordsAdvance,
    RecordsPublish, RecordsSite, RefMerge, RefOrigin, RefRestoreOutcome, RefSavepoint,
    RegistrationPrune, RemoteBranchCopies, RemoteBranchMove, RemoteDefaultBranch, RemoteHandle,
    RemoteNameTaken, RemoteRef, RemoteTip, RemoteTracking, Replace, ResolvedEndpoint,
    ResolvedRevisionId, SavepointListing, Site, SubmoduleBirth, SubmoduleCheckout, ToolReadiness,
    TrackedFile, TrackedFileKind, TreeMerges, UnconfirmedRemote, Upstream, UrlRemote, Vcs,
    VcsAction, VcsError, VerifiedRestoreOutcome, WorkspacePopulation,
};
use crate::vcs::{Attribution, Integrates, KeptWork};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

mod argv;
mod backup;
mod merge;
mod pick;
mod records;

use argv::{Argv, GitArg, GitCommand, NotUtf8};

/// A `git` command with all inherited `GIT_*` environment variables
/// stripped.
///
/// Private to this module, which is what keeps git's argv here: a frame
/// elsewhere cannot assemble one, so `error[E0603]` answers "did anyone spell
/// a git command outside the seam" without a text scan having to.
fn git_command() -> GitCommand {
    GitCommand::new()
}

impl From<NotUtf8> for VcsError {
    fn from(e: NotUtf8) -> VcsError {
        VcsError::Io {
            ctx: format!("{} {} is not valid UTF-8", e.subject(), e.path().display()),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("non-utf8 {}", e.subject()),
            ),
        }
    }
}

/// [`git_command`] for a `#[cfg(test)]` module that drives a real repo to set
/// up a fixture.
///
/// **A deliberate exemption, and the seam is wider by exactly this much.** A
/// test module in any file can assemble git argv through here, so the
/// compile-time closure above holds for production and not for tests. That is
/// the same allowance the seam already makes for [`git_vcs`], and for the same
/// reason: a fixture has to build the repo state the code under test reads,
/// and routing that through the trait would test the trait against itself.
/// It is `#[cfg(test)]` rather than `pub(crate)` so the exemption cannot reach
/// the shipped binary — widening it cannot widen production's reach.
#[cfg(test)]
pub(crate) fn git_command_in_test() -> std::process::Command {
    git_command().into_command()
}

/// The two line-ending configurations a checkout meets in practice. A fixture
/// pins one in its repository's own config, so the global config of the
/// machine running the test cannot choose for it.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineEndings {
    /// `core.autocrlf=false` and `core.eol=lf`: text checks out as committed.
    Verbatim,
    /// `core.autocrlf=true`, Git for Windows' default: text checks out with
    /// CRLF line endings, whatever `core.eol` says.
    Converted,
}

#[cfg(test)]
impl LineEndings {
    pub(crate) fn pin(self, repo: &Path) {
        let settings: &[(&str, &str)] = match self {
            Self::Verbatim => &[("core.autocrlf", "false"), ("core.eol", "lf")],
            Self::Converted => &[("core.autocrlf", "true")],
        };
        for (key, value) in settings {
            let out = git_command()
                .args(["config", key, value])
                .current_dir(repo)
                .output()
                .expect("run git config");
            assert!(out.status.success(), "git config {key} {value} failed");
        }
    }
}

/// Each named test function, which takes a [`LineEndings`], runs as one test
/// under each configuration: `verbatim::<name>` and `converted::<name>`.
#[cfg(test)]
macro_rules! under_both_line_endings {
    ($($(#[$attr:meta])* $name:ident),* $(,)?) => {
        mod verbatim {
            $(
                $(#[$attr])*
                #[test]
                fn $name() {
                    super::$name($crate::git::LineEndings::Verbatim)
                }
            )*
        }
        mod converted {
            $(
                $(#[$attr])*
                #[test]
                fn $name() {
                    super::$name($crate::git::LineEndings::Converted)
                }
            )*
        }
    };
}
#[cfg(test)]
pub(crate) use under_both_line_endings;

#[cfg(test)]
pub(crate) use argv::speech;

/// Each named test function runs as one test with git's locale pinned and
/// one with git unpinned and speaking German: `pinned::<name>` and
/// `translated::<name>`.
#[cfg(test)]
macro_rules! in_both_languages {
    ($($name:ident),* $(,)?) => {
        mod pinned {
            $(
                #[test]
                fn $name() {
                    super::$name()
                }
            )*
        }
        mod translated {
            $(
                #[test]
                fn $name() {
                    $crate::git::speech::in_translated_git(super::$name)
                }
            )*
        }
    };
}

/// What git names the first remote of a repo that has none. It names a remote
/// being added to such a repo and stands for no role: every use of a remote
/// resolves the role it plays.
const GIT_FIRST_REMOTE_NAME: &str = "origin";

const ATTRIBUTES_FILE: &str = ".gitattributes";

/// The local-config key recording the project repo's url remote.
const PROJECT_URL_REMOTE_KEY: &str = "rwv.url-remote";

/// `out`, which git printed for `args`, as the commit id it must be.
/// `message` with each annotation appended as a trailer, in a paragraph of
/// its own so git reads them as the message's trailer block.
fn with_trailers(message: &str, annotations: &[Annotation]) -> String {
    if annotations.is_empty() {
        return message.to_owned();
    }
    let mut text = message.trim_end().to_owned();
    text.push_str("\n\n");
    for annotation in annotations {
        text.push_str(&format!("{}: {}\n", annotation.key, annotation.value));
    }
    text
}

fn commit_id_printed(out: &str, args: &[&str], repo: &Path) -> Result<CommitId, VcsError> {
    CommitId::parse(out.trim()).map_err(|e| VcsError::CommandFailed {
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        repo: repo.to_path_buf(),
        stderr: format!("{e:#}"),
    })
}

fn remote_tracking_namespace(remote: &RemoteHandle) -> String {
    format!("refs/remotes/{}/", remote.name())
}

/// The only spelling of a remote-tracking ref handed to git. The short
/// `<remote>/<branch>` is never one: git resolves it to a local branch or tag
/// of that name before `refs/remotes/<remote>/<branch>`.
fn remote_tracking(remote: &RemoteHandle, branch: &str) -> String {
    format!("{}{branch}", remote_tracking_namespace(remote))
}

/// The segment of `url` that names its owner: the one before the repository's
/// own, whatever the url's scheme. `None` for a url with a single segment.
fn owner_segment(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let path = match trimmed.split_once("://") {
        Some((_, rest)) if rest.starts_with('/') => rest,
        Some((_, rest)) => rest.split_once('/').map_or("", |(_, path)| path),
        None => match (trimmed.find(':'), trimmed.find('/')) {
            (Some(colon), Some(slash)) if colon < slash => &trimmed[colon + 1..],
            (Some(colon), None) => &trimmed[colon + 1..],
            _ => trimmed,
        },
    };
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let owner = segments.get(segments.len().checked_sub(2)?)?;
    let name: String = owner
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches(|c| c == '-' || c == '.').to_owned();
    (!name.is_empty() && !name.ends_with(".lock")).then_some(name)
}

/// Where the tip a remote-tracking branch was rewritten away from is kept:
/// `<remote>/<branch>/<tip>` below it.
const SUPERSEDED_NAMESPACE: &str = "refs/rwv/superseded/";

const LOCAL_BRANCH_NAMESPACE: &str = "refs/heads/";

/// The only spelling of a local branch handed to git where git resolves a
/// revision or refspec. A bare `<branch>` there resolves to a tag of that name
/// before `refs/heads/<branch>`.
fn local_branch(name: &str) -> String {
    format!("{LOCAL_BRANCH_NAMESPACE}{name}")
}

fn local_branch_tip_args(name: &str) -> Vec<String> {
    vec![
        "rev-parse".to_owned(),
        "--verify".to_owned(),
        "--quiet".to_owned(),
        format!("{}^{{commit}}", local_branch(name)),
    ]
}

// ---------------------------------------------------------------------------
// The lock merge driver
// ---------------------------------------------------------------------------
//
// A git merge driver has two halves. A `.gitattributes` line assigns a driver
// to a path by name, and travels with commits. A `merge.<name>.driver` config
// entry names the command git runs, and travels with nothing: a clone carries
// none, while a linked worktree shares its clone's. A name no config git can
// see defines falls back to git's textual merge, silently.

/// The driver name `rwv.lock` is assigned in `.gitattributes`.
pub const RWV_MERGE_DRIVER_NAME: &str = "rwv-lock";

/// The config key defining the driver's command.
pub const RWV_MERGE_DRIVER_CONFIG_KEY: &str = "merge.rwv-lock.driver";

const RWV_MERGE_DRIVER_NAME_KEY: &str = "merge.rwv-lock.name";
const RWV_MERGE_DRIVER_NAME_DESC: &str = "the rwv.lock merge driver";

/// The command the driver's config names. git runs it through its shell from
/// the repo's top directory, finding `rwv` on `PATH` as it finds git-lfs for
/// its filters, so no absolute path is recorded to go stale and nothing in the
/// command needs quoting; git substitutes the three temporary files itself.
pub const RWV_MERGE_DRIVER_COMMAND: &str = "rwv lock --merge-driver %O %A %B";

/// The driver name a repo assigns to paths it declares derived. Nothing
/// defines it durably: rwv's own replays define it inline as a side-pick
/// keeping the target's copy, and the repo's gates force the regeneration.
pub const DERIVED_CONTENT_DRIVER_NAME: &str = "rwv-ours";
const DERIVED_CONTENT_DRIVER_CONFIG_KEY: &str = "merge.rwv-ours.driver";

/// Driver names earlier releases assigned to the lock. A lock line naming one
/// is migrated, never honored: each kept one side of every lock merge.
const SUPERSEDED_LOCK_DRIVERS: [&str; 2] = [DERIVED_CONTENT_DRIVER_NAME, "ours"];

/// The config earlier releases planted in a project repo for the first of
/// those names.
const SUPERSEDED_DRIVER_KEYS: [&str; 2] =
    [DERIVED_CONTENT_DRIVER_CONFIG_KEY, "merge.rwv-ours.name"];

/// Namespace prefix every driver name rwv defines falls under. Only rwv
/// writes `merge=rwv-…` and only rwv defines `merge.rwv-….driver`, so a
/// `.gitattributes` line naming a driver under this prefix that rwv does not
/// define is one nothing else will define either — `check::phantom_merge_drivers_in`
/// uses the prefix to find such lines.
pub const RWV_MERGE_DRIVER_PREFIX: &str = "rwv-";

fn lock_attribute_line(driver: &str) -> String {
    format!("{} merge={driver}", crate::manifest::LockFile::FILE_NAME)
}

/// Whether `line` assigns a driver an earlier release wrote to the lock.
pub(crate) fn is_superseded_lock_attribute(line: &str) -> bool {
    SUPERSEDED_LOCK_DRIVERS
        .iter()
        .any(|driver| line.trim() == lock_attribute_line(driver))
}

/// How a `.gitattributes` text declares the lock's merge driver.
fn lock_attribute_in(text: Option<&str>) -> LockDriverPart {
    let lines = || text.into_iter().flat_map(str::lines);
    if lines().any(is_superseded_lock_attribute) {
        return LockDriverPart::Superseded;
    }
    let current = lock_attribute_line(RWV_MERGE_DRIVER_NAME);
    if lines().any(|line| line.trim() == current) {
        LockDriverPart::Current
    } else {
        LockDriverPart::Absent
    }
}

/// `text` with every superseded lock line dropped and the current one
/// present once; `None` when it already is exactly that.
fn declare_lock_driver_in(text: Option<&str>) -> Option<String> {
    let existing = text.unwrap_or("");
    if lock_attribute_in(text) == LockDriverPart::Current {
        return None;
    }
    let current = lock_attribute_line(RWV_MERGE_DRIVER_NAME);
    let mut out = String::with_capacity(existing.len() + current.len() + 1);
    let mut wrote = false;
    for line in existing.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if is_superseded_lock_attribute(bare) || bare.trim() == current {
            if !wrote {
                out.push_str(&current);
                out.push_str(&line[bare.len()..]);
                wrote = true;
            }
            continue;
        }
        out.push_str(line);
    }
    if !wrote {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&current);
        out.push('\n');
    } else if !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// The `-c` pair under which a git command keeps the lock merge driver from
/// running: the driver resolves to `true`, which leaves the side git already
/// holds. rwv writes the lock of every merge and replay it makes itself, so
/// its own commands neither depend on `rwv` being on `PATH` nor stop on a lock
/// the driver would conflict.
fn lock_driver_held_args() -> [String; 2] {
    [
        "-c".to_owned(),
        format!("{RWV_MERGE_DRIVER_CONFIG_KEY}=true"),
    ]
}

/// Translate a [`DerivedContentPolicy`] into the leading `git` arguments
/// that put it in force for a single invocation.
///
/// [`DerivedContentResolution::KeepTargetSide`] defines the derived-content
/// driver, and holds the lock merge driver, at `true`: the shell command that
/// exits 0 without touching the file, leaving the version already on the side
/// being replayed onto. A commit touching only such paths replays as an empty
/// patch. [`DerivedContentResolution::VcsDefault`] contributes no arguments,
/// so the repo's own config decides.
fn derived_content_git_args(policy: DerivedContentPolicy) -> Vec<String> {
    match policy.resolution() {
        DerivedContentResolution::KeepTargetSide => {
            let mut args = vec![
                "-c".to_owned(),
                format!("{DERIVED_CONTENT_DRIVER_CONFIG_KEY}=true"),
            ];
            args.extend(lock_driver_held_args());
            args
        }
        DerivedContentResolution::VcsDefault => Vec::new(),
    }
}

/// Split a git `-z` stream into its records.
///
/// Git terminates — rather than separates — each record under `-z`, so the
/// trailing empty tail is an artifact of the format and not a record. `-z` is
/// also what turns quoting off, so a record is the pathname's own bytes.
fn nul_terminated_fields(stream: &str) -> Vec<String> {
    stream
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The value of `key` in `repo`'s config at `scope` (`None` reads every scope
/// git reads), `None` when unset.
///
/// Checked ahead of the read: a repo lacking the key and a directory that
/// isn't a repo at all both exit 1.
fn config_value(repo: &Path, scope: Option<&str>, key: &str) -> Result<Option<String>, VcsError> {
    if !GitVcs.is_repo(repo) {
        return Err(VcsError::NotARepo(repo.to_path_buf()));
    }
    let mut args = vec!["config"];
    args.extend(scope);
    args.extend(["--get", key]);
    Ok(GitVcs::run_or_absent(args, repo, Absent::ConfigKey)?
        .map(|value| value.trim_end().to_owned()))
}

fn lock_driver_config_part(repo: &Path) -> Result<LockDriverPart, VcsError> {
    for key in SUPERSEDED_DRIVER_KEYS {
        if config_value(repo, Some("--local"), key)?.is_some() {
            return Ok(LockDriverPart::Superseded);
        }
    }
    Ok(
        match config_value(repo, None, RWV_MERGE_DRIVER_CONFIG_KEY)?.as_deref() {
            None => LockDriverPart::Absent,
            Some(RWV_MERGE_DRIVER_COMMAND) => LockDriverPart::Current,
            Some(_) => LockDriverPart::Superseded,
        },
    )
}

/// The commits a merge in progress in `repo` joins, read from what git hands
/// a merge driver it runs.
///
/// `git merge` (and `git pull`) export `GITHEAD_<id>=<name>` for each head it
/// merges in, before `MERGE_HEAD` is written. A rebase, cherry-pick, revert or
/// stash apply exports none, and its three sides are not two heads and their
/// base; `merge-tree` leaves no state at all. Exactly one exported head, with
/// no sequencer state beside it, is the only case where both commits are named.
fn merging_heads(repo: &Path) -> Option<(CommitId, CommitId)> {
    if GitVcs::mid_op_state(repo).is_some_and(|op| op != MidOperation::Conflict(ConflictOp::Merge))
    {
        return None;
    }
    let mut heads = std::env::vars_os().filter_map(|(name, _)| {
        name.to_str()?
            .strip_prefix("GITHEAD_")
            .and_then(|id| CommitId::parse(id).ok())
    });
    let theirs = heads.next()?;
    if heads.next().is_some() {
        return None;
    }
    let ours = GitVcs::run_or_absent(
        ["rev-parse", "--verify", "-q", "HEAD^{commit}"],
        repo,
        Absent::Revision,
    )
    .ok()
    .flatten()
    .and_then(|id| CommitId::parse(id.trim()).ok())?;
    Some((ours, theirs))
}

/// Git-based version control operations.
///
/// Private to this module: outside it the git backend is reachable only as a
/// [`Vcs`] handle from [`git_vcs`], so no call site can name
/// a backend it could not be handed a different one of.
struct GitVcs;

// ---------------------------------------------------------------------------
// On-disk layout
// ---------------------------------------------------------------------------
//
// Everything below reads git's own directory convention. It lives here so that
// no other module has to spell `.git`, and so the derivations that follow a
// gitlink exist once rather than once per caller.

/// Entry git places at the root of a checkout — a directory in a canonical
/// store, a file holding a gitlink in a linked worktree.
pub const GIT_DIR_ENTRY_NAME: &str = ".git";

/// Where a checkout rooted at `dir` keeps its store.
///
/// A construction, not a probe: the answer is the same whether or not `dir` is
/// a repo at all. Callers comparing an observed store against where one
/// *should* sit want this; callers asking whether a repo lives there want
/// `has_git_dir`, andXX
pub fn store_path_in(dir: &Path) -> PathBuf {
    dir.join(GIT_DIR_ENTRY_NAME)
}

/// The git directory a checkout's `.git` entry names, and whether the checkout
/// owns it.
pub enum GitDirLink {
    /// `.git` is a directory: this checkout is a store in its own right.
    Owned(PathBuf),
    /// `.git` is a file holding `gitdir:`: this checkout's store is elsewhere.
    Linked(PathBuf),
}

/// Resolve the `.git` entry at `dir` to the git directory it names.
///
/// Purely lexical past reading the entry itself — the named directory is not
/// required to exist. That is what lets a caller ask about a worktree whose
/// store has been deleted out of band, which is a question only answerable
/// before the answer is checked against the filesystem.
///
/// `None` when `dir` has no `.git` entry, or the entry is a file that does not
/// carry a `gitdir:` line. No upward walk: `dir` is checked directly.
pub fn git_dir_link(dir: &Path) -> Option<GitDirLink> {
    let entry = store_path_in(dir);
    if entry.is_dir() {
        return Some(GitDirLink::Owned(entry));
    }
    if !entry.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&entry).ok()?;
    let target = content
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    let target = PathBuf::from(target);
    Some(GitDirLink::Linked(if target.is_absolute() {
        target
    } else {
        dir.join(target)
    }))
}

/// The git directory `git_dir` shares with every other worktree of the same
/// store, read from the `commondir` file git writes beside a per-worktree dir.
///
/// `None` when there is no readable `commondir`, which is two different
/// situations a caller must tell apart itself: `git_dir` is a store in its own
/// right (`--separate-git-dir` writes no `commondir`), or the store it belongs
/// to is gone and took the file with it.
pub fn commondir_target(git_dir: &Path) -> Option<PathBuf> {
    let commondir = git_dir.join("commondir");
    let content = std::fs::read_to_string(&commondir).ok()?;
    let target = PathBuf::from(content.trim());
    Some(if target.is_absolute() {
        target
    } else {
        git_dir.join(target)
    })
}

/// The clone directory a linked worktree at `repo` names through its link,
/// read from the filesystem alone, so it answers when that clone is gone;
/// `None` for a checkout that owns its store, or one with no readable link.
///
/// The store is resolved from `commondir` when git's record of it survives,
/// and from the `<store>/worktrees/<name>` layout when it does not. That
/// second arm is not a shortcut: the `commondir` file lives inside the
/// clone, so it is gone in precisely the situation a caller asks about.
pub(crate) fn linked_clone_dir(repo: &Path) -> Option<PathBuf> {
    let git_dir = match git_dir_link(repo)? {
        GitDirLink::Owned(_) => return None,
        GitDirLink::Linked(git_dir) => git_dir,
    };
    let store = commondir_target(&git_dir).or_else(|| store_above_worktree_dir(&git_dir))?;
    Some(store.parent()?.to_path_buf())
}

/// The store directory above a per-worktree git dir, assuming the layout `git
/// worktree add` writes: `<store>/worktrees/<name>` → `<store>`.
///
/// Lexical, and an assumption rather than a reading — [`commondir_target`] is
/// what git actually records, and this is what remains when that file cannot
/// be read. The result need not exist, which is the point: a caller
/// diagnosing a *deleted* store has nothing else left to go on.
pub fn store_above_worktree_dir(git_dir: &Path) -> Option<PathBuf> {
    Some(git_dir.parent()?.parent()?.to_path_buf())
}

/// True when `checkout` is a linked worktree rather than a canonical store.
///
/// The test is git's own on-disk convention: a linked worktree's `.git` is a
/// *file* holding a gitlink into `.git/worktrees/<name>`, where a canonical
/// store's is a directory. `git worktree remove` refuses on the latter with
/// "is a main working tree", so a caller that has to tell them apart is asking
/// a question about git's layout and nothing else.
pub(crate) fn is_linked_worktree(checkout: &Path) -> bool {
    let dot_git = store_path_in(checkout);
    dot_git.exists() && dot_git.is_file()
}

// ---------------------------------------------------------------------------
// Drift probes: where a checkout's branch moved from, and each path against it
// ---------------------------------------------------------------------------

/// [`Vcs::left_behind_at`] for git.
///
/// A worktree's `HEAD` reflog gains an entry for each move made in that
/// worktree, `update-ref` of its checked-out branch included, and none for a
/// move of the same branch made from another worktree; the newest entry's
/// commit is where the worktree last put HEAD.
fn left_behind_at(repo: &Path) -> Result<Option<CommitId>, VcsError> {
    let HeadAttachment::Attached(_) = GitVcs.head_attachment(repo)? else {
        return Ok(None);
    };
    let recorded = GitVcs::run_trimmed(
        [
            "log",
            "-g",
            "-n1",
            "--no-show-signature",
            "--format=%H",
            "HEAD",
        ],
        repo,
    )?;
    let Ok(recorded) = CommitId::parse(&recorded) else {
        return Ok(None);
    };
    let head = GitVcs.head_revision(repo)?;
    if *head.id() == recorded {
        return Ok(None);
    }
    let forward = GitVcs.is_ancestor(repo, &ResolvedRevisionId::from(recorded.clone()), &head)?;
    Ok(forward.then_some(recorded))
}

/// Each path `diff-index` reports between `rev` and the index (`cached`) or
/// the working tree, with its status letter.
fn changed_against(
    repo: &Path,
    rev: &str,
    cached: bool,
) -> Result<BTreeMap<String, String>, VcsError> {
    let mut args = vec!["diff-index", "--name-status", "-z", "--no-renames"];
    if cached {
        args.push("--cached");
    }
    args.push(rev);
    let fields = nul_terminated_fields(&GitVcs::run(&args, repo)?);
    let mut changed = BTreeMap::new();
    for record in fields.chunks(2) {
        let [status, path] = record else {
            return Err(VcsError::Io {
                ctx: format!("unpaired diff-index record in {}", repo.display()),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, "odd -z stream"),
            });
        };
        changed.insert(path.clone(), status.clone());
    }
    Ok(changed)
}

/// [`Vcs::path_standings`] for git.
///
/// `diff-index` against a tree reads a path the index lacks as absent from
/// the working tree whatever is on disk there, so a path it reports deleted
/// that something on disk still occupies holds [`Holds::Other`]. It also
/// reports a file whose stat data no longer matches its index entry as
/// changed without reading it, so the stat data is refreshed first, as
/// `git status` does; the refresh writes no content to the index.
fn path_standings(repo: &Path, left: Option<&CommitId>) -> Result<Vec<PathStanding>, VcsError> {
    let _ = git_command()
        .args(["update-index", "-q", "--refresh"])
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let file_vs_head = changed_against(repo, "HEAD", false)?;
    let index_vs_head = changed_against(repo, "HEAD", true)?;
    let (moved, file_vs_left, index_vs_left) = match left {
        Some(left) => (
            nul_terminated_fields(&GitVcs::run(
                [
                    "diff-tree",
                    "-r",
                    "-z",
                    "--no-renames",
                    "--name-only",
                    left.as_str(),
                    "HEAD",
                ],
                repo,
            )?)
            .into_iter()
            .collect::<BTreeSet<String>>(),
            changed_against(repo, left.as_str(), false)?,
            changed_against(repo, left.as_str(), true)?,
        ),
        None => Default::default(),
    };
    let side =
        |vs_head: &BTreeMap<String, String>, vs_left: &BTreeMap<String, String>, path: &str| {
            if [vs_head.get(path), vs_left.get(path)]
                .into_iter()
                .flatten()
                .any(|status| status == "U")
            {
                Holds::Other
            } else if !vs_head.contains_key(path) {
                Holds::Head
            } else if left.is_some() && !vs_left.contains_key(path) {
                Holds::LeftTip
            } else {
                Holds::Other
            }
        };
    let paths: BTreeSet<&String> = file_vs_head.keys().chain(index_vs_head.keys()).collect();
    Ok(paths
        .into_iter()
        .map(|path| {
            let mut file = side(&file_vs_head, &file_vs_left, path);
            let occupied = std::fs::symlink_metadata(repo.join(path)).is_ok();
            if file_vs_head.get(path).is_some_and(|s| s == "D") && occupied {
                file = Holds::Other;
            }
            PathStanding {
                path: path.clone(),
                moved: moved.contains(path),
                index: side(&index_vs_head, &index_vs_left, path),
                file,
            }
        })
        .collect())
}

/// [`Vcs::refresh_paths_from_head`] for git. `checkout --no-overlay` removes a
/// path `HEAD` lacks where plain `checkout` would leave it.
fn refresh_paths_from_head(
    repo: &Path,
    files: &[String],
    index_entries: &[String],
) -> Result<(), VcsError> {
    if !files.is_empty() {
        let mut args = vec![
            "--literal-pathspecs",
            "checkout",
            "--no-overlay",
            "HEAD",
            "--",
        ];
        args.extend(files.iter().map(String::as_str));
        GitVcs::run(&args, repo)?;
    }
    if !index_entries.is_empty() {
        let mut args = vec!["--literal-pathspecs", "reset", "-q", "HEAD", "--"];
        args.extend(index_entries.iter().map(String::as_str));
        GitVcs::run(&args, repo)?;
    }
    Ok(())
}

/// True when the repo has any index or working-tree change against HEAD.
///
/// `None` when the probe itself failed — the caller cannot read that as clean.
pub fn repo_is_dirty(repo: &Path) -> Option<bool> {
    GitVcs.has_uncommitted_changes(repo).ok()
}

/// The only way to obtain a git backend from outside this module.
///
/// The backend type itself is private, so a caller that wants git behavior
/// must accept it as a [`Vcs`] handle and can be handed a different one.
pub fn git_vcs() -> Box<dyn crate::vcs::Vcs> {
    Box::new(GitVcs)
}

impl GitVcs {
    /// Run a git command in `dir` and return stdout exactly as git wrote it.
    ///
    /// Untrimmed: a `-z` stream's first record, or a porcelain line's leading
    /// column, may begin with whitespace. Callers reading a single value want
    /// [`GitVcs::run_trimmed`].
    ///
    /// Maps process I/O failure to [`VcsError::Io`] and non-zero exit to
    /// [`VcsError::CommandFailed`] with the args and stderr captured. The
    /// stderr is carried for the operator, never matched: a caller that tells
    /// failures apart asks by exit status, a machine format or the
    /// repository's state.
    fn run(args: impl Into<Argv>, dir: &Path) -> Result<String, VcsError> {
        let args = args.into();
        let output = git_command()
            .args(&args)
            .current_dir(dir)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(VcsError::CommandFailed {
                args: args.into_words(),
                repo: dir.to_path_buf(),
                stderr,
            });
        }

        String::from_utf8(output.stdout).map_err(|_| VcsError::CommandFailed {
            args: args.into_words(),
            repo: dir.to_path_buf(),
            stderr: "git output not valid UTF-8".to_string(),
        })
    }

    fn commit_through_own_index(
        repo: &Path,
        index: &str,
        born: bool,
        paths: &[&str],
        message: &str,
    ) -> Result<bool, VcsError> {
        let in_index = |args: &[&str], input: &[u8]| {
            Self::run_bytes_in(args, repo, input, &[("GIT_INDEX_FILE", index)])
        };
        if born {
            in_index(&["read-tree", "HEAD"], &[])?;
        }
        let mut listing = vec!["ls-files", "-s", "-z", "--full-name", "--"];
        listing.extend_from_slice(paths);
        let staged = Self::run_bytes_in(&listing, repo, &[], &[])?;
        let prefix = Self::run_trimmed(["rev-parse", "--show-prefix"], repo)?;
        let present: Vec<String> = staged
            .split(|byte| *byte == 0)
            .filter_map(|entry| {
                let tab = entry.iter().position(|byte| *byte == b'\t')?;
                let full = String::from_utf8_lossy(&entry[tab + 1..]).into_owned();
                Some(full.strip_prefix(&prefix).unwrap_or(&full).to_owned())
            })
            .collect();
        if !present.is_empty() {
            in_index(&["update-index", "-z", "--index-info"], &staged)?;
        }
        let absent: Vec<&str> = paths
            .iter()
            .copied()
            .filter(|path| !present.iter().any(|p| p == path))
            .collect();
        if !absent.is_empty() {
            let mut remove = vec!["update-index", "--force-remove", "--"];
            remove.extend_from_slice(&absent);
            in_index(&remove, &[])?;
        }
        let changed = if born {
            in_index(&["diff-index", "--cached", "--name-only", "HEAD"], &[])?
        } else {
            in_index(&["ls-files"], &[])?
        };
        if changed.iter().all(u8::is_ascii_whitespace) {
            return Ok(false);
        }
        in_index(&["commit", "-q", "-m", message], &[])?;
        Ok(true)
    }

    fn run_trimmed(args: impl Into<Argv>, dir: &Path) -> Result<String, VcsError> {
        Self::run(args, dir).map(|s| s.trim().to_string())
    }

    /// [`GitVcs::run`] for a command that answers "nothing there" with the
    /// exit status `absent`: `None` for that answer, an error for any other
    /// failure.
    fn run_or_absent(
        args: impl Into<Argv>,
        dir: &Path,
        absent: Absent,
    ) -> Result<Option<String>, VcsError> {
        let args = args.into();
        let output = git_command()
            .args(&args)
            .current_dir(dir)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        if output.status.code() == Some(absent.status()) {
            return Ok(None);
        }
        if !output.status.success() {
            return Err(VcsError::CommandFailed {
                args: args.into_words(),
                repo: dir.to_path_buf(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        String::from_utf8(output.stdout)
            .map(Some)
            .map_err(|_| VcsError::CommandFailed {
                args: args.into_words(),
                repo: dir.to_path_buf(),
                stderr: "git output not valid UTF-8".to_string(),
            })
    }

    fn run_trimmed_or_absent(
        args: impl Into<Argv>,
        dir: &Path,
        absent: Absent,
    ) -> Result<Option<String>, VcsError> {
        Self::run_or_absent(args, dir, absent).map(|s| s.map(|s| s.trim().to_string()))
    }

    /// [`GitVcs::run`] for a command whose stderr is its progress report:
    /// returns what git wrote there once it has succeeded.
    fn run_logged(args: impl Into<Argv>, dir: &Path) -> Result<String, VcsError> {
        let args = args.into();
        let output = git_command()
            .args(&args)
            .current_dir(dir)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if output.status.success() {
            return Ok(stderr);
        }
        Err(VcsError::CommandFailed {
            args: args.into_words(),
            repo: dir.to_path_buf(),
            stderr,
        })
    }

    /// Whether `repo`'s config holds any `branch.<branch>.*` key.
    fn has_branch_section(repo: &Path, branch: &str) -> Result<bool, VcsError> {
        let Some(listing) = Self::run_or_absent(
            ["config", "-z", "--name-only", "--get-regexp", r"^branch\."],
            repo,
            Absent::ConfigKey,
        )?
        else {
            return Ok(false);
        };
        let prefix = format!("branch.{branch}.");
        Ok(listing.split_terminator('\0').any(|key| {
            key.strip_prefix(&prefix)
                .is_some_and(|variable| !variable.contains('.'))
        }))
    }

    /// The value of config `key` in `repo`, or `None` when it is unset.
    fn config_get(repo: &Path, key: &str) -> Result<Option<String>, VcsError> {
        Ok(
            Self::run_trimmed_or_absent(["config", "--get", key], repo, Absent::ConfigKey)?
                .filter(|value| !value.is_empty()),
        )
    }

    /// The names of `repo`'s remotes.
    /// A full local or remote-tracking ref name as the branch it is.
    /// `remotes` are tried longest first, since a remote's name may hold `/`.
    fn branch_ref_of(refname: &str, remotes: &[String]) -> crate::branch_ref::BranchRef {
        use crate::branch_ref::BranchRef;
        if let Some(local) = refname.strip_prefix(LOCAL_BRANCH_NAMESPACE) {
            return BranchRef::Local(RawRefName::new(local));
        }
        let tracked = refname.strip_prefix("refs/remotes/").unwrap_or(refname);
        let mut longest_first: Vec<&String> = remotes.iter().collect();
        longest_first.sort_by_key(|name| std::cmp::Reverse(name.len()));
        let (remote, name) = longest_first
            .iter()
            .find_map(|r| {
                tracked
                    .strip_prefix(r.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(|name| (r.as_str(), name))
            })
            .or_else(|| tracked.split_once('/'))
            .unwrap_or(("", tracked));
        BranchRef::Remote {
            remote: RemoteHandle::new(remote),
            name: RawRefName::new(name),
        }
    }

    fn remote_names(repo: &Path) -> Result<Vec<String>, VcsError> {
        Ok(Self::run_trimmed(["remote"], repo)?
            .lines()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// The remote of a repo that has exactly one: the remote a clone just
    /// made.
    fn only_remote(repo: &Path) -> Result<RemoteHandle, VcsError> {
        match Self::remote_names(repo)?.as_slice() {
            [only] => Ok(RemoteHandle::new(only.clone())),
            names => Err(VcsError::CommandFailed {
                args: vec!["remote".into()],
                repo: repo.to_path_buf(),
                stderr: format!(
                    "expected the one remote a clone makes, found {}",
                    names.len()
                ),
            }),
        }
    }

    /// The remote-tracking refs, one per remote that has it, of `branch`.
    fn remote_counterparts(repo: &Path, branch: &str) -> Result<Vec<String>, VcsError> {
        let mut found = Vec::new();
        for name in Self::remote_names(repo)? {
            let qualified = remote_tracking(&RemoteHandle::new(name), branch);
            let status = git_command()
                .args(["rev-parse", "--verify", "--quiet", &qualified])
                .current_dir(repo)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|e| VcsError::Io {
                    ctx: format!(
                        "failed to spawn git rev-parse --verify --quiet {qualified} in {}",
                        repo.display()
                    ),
                    source: e,
                })?;
            if status.success() {
                found.push(qualified);
            }
        }
        Ok(found)
    }

    /// Every remote-tracking branch of `repo`, remote-qualified, at its tip.
    /// A remote's `HEAD` is a symbolic ref that follows a branch, not a branch.
    fn remote_tracking_tips(repo: &Path) -> Result<BTreeMap<String, String>, VcsError> {
        let listing = Self::run(
            [
                "for-each-ref",
                "--format=%(objectname) %(refname) %(symref)",
                "refs/remotes/",
            ],
            repo,
        )?;
        Ok(listing
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let (tip, refname) = (fields.next()?, fields.next()?);
                if fields.next().is_some() {
                    return None;
                }
                Some((
                    refname.strip_prefix("refs/remotes/")?.to_owned(),
                    tip.to_owned(),
                ))
            })
            .collect())
    }

    /// Whether `to` descends from `from`, both read as commit ids in `repo`.
    fn descends(repo: &Path, from: &str, to: &str) -> Result<bool, VcsError> {
        let status = git_command()
            .args(["merge-base", "--is-ancestor", from, to])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git merge-base in {}", repo.display()),
                source: e,
            })?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(VcsError::CommandFailed {
                args: vec![
                    "merge-base".to_owned(),
                    "--is-ancestor".to_owned(),
                    from.to_owned(),
                    to.to_owned(),
                ],
                repo: repo.to_path_buf(),
                stderr: format!("git merge-base exited {status}"),
            }),
        }
    }

    /// Keep `tip`, which `branch` no longer reaches, under the superseded
    /// namespace.
    fn record_superseded_tip(repo: &Path, branch: &str, tip: &str) -> Result<(), VcsError> {
        let refname = format!("{SUPERSEDED_NAMESPACE}{branch}/{tip}");
        Self::run(["update-ref", &refname, tip], repo)?;
        Ok(())
    }
}

/// How long a git command that died on a peer's unfinished worktree
/// registration is retried before its failure is believed.
const UNWRITTEN_REGISTRATION_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

const UNWRITTEN_REGISTRATION_POLL: std::time::Duration = std::time::Duration::from_millis(10);

impl GitVcs {
    /// [`GitVcs::run`] for a git command that reads the repo's worktree
    /// registrations before it changes anything.
    fn run_awaiting_peers(args: impl Into<Argv>, dir: &Path) -> Result<String, VcsError> {
        let args = args.into();
        Self::awaiting_peer_registration(dir, None, UNWRITTEN_REGISTRATION_WAIT, || {
            Self::run(&args, dir)
        })
    }

    fn run_trimmed_awaiting_peers(args: impl Into<Argv>, dir: &Path) -> Result<String, VcsError> {
        Self::run_awaiting_peers(args, dir).map(|s| s.trim().to_string())
    }

    /// [`GitVcs::run_awaiting_peers`] for a `worktree add` registering
    /// `adding`.
    fn add_awaiting_peers(
        args: impl Into<Argv>,
        dir: &Path,
        adding: &Path,
    ) -> Result<String, VcsError> {
        let args = args.into();
        Self::awaiting_peer_registration(dir, Some(adding), UNWRITTEN_REGISTRATION_WAIT, || {
            Self::run(&args, dir)
        })
    }

    /// Run `attempt` again for up to `wait` while it fails because a peer's
    /// `git worktree add` has not finished its registration.
    ///
    /// `git worktree add` creates the new worktree's registration under the
    /// shared repo and writes its `commondir` a moment later, in two steps. A
    /// git command that enumerates the repo's worktrees in between reads the
    /// file empty and dies. The writer finishes in microseconds on a local
    /// disk and in milliseconds where file operations are slow. So a failure
    /// can be the peer's only while a registration is unwritten, or when one
    /// that was not written before the attempt has appeared since; any other
    /// failure is believed at once. `adding` is the worktree the attempt
    /// itself registers, whose registration is no peer's.
    ///
    /// Only an `attempt` that has changed nothing when it dies can be run
    /// again as it was.
    fn awaiting_peer_registration<T>(
        dir: &Path,
        adding: Option<&Path>,
        wait: std::time::Duration,
        mut attempt: impl FnMut() -> Result<T, VcsError>,
    ) -> Result<T, VcsError> {
        let Some(registrations) = Registrations::of(dir) else {
            return attempt();
        };
        let deadline = std::time::Instant::now() + wait;
        let mut looked_clean = false;
        loop {
            let written_before = registrations.written();
            let failure = match attempt() {
                Err(failure @ VcsError::CommandFailed { .. }) => failure,
                result => return result,
            };
            let peers: Vec<PathBuf> = registrations
                .present()
                .into_iter()
                .filter(|r| adding.is_none_or(|adding| !Registrations::registers(r, adding)))
                .collect();
            if let Some(unwritten) = peers.iter().find(|r| !Registrations::is_written(r)) {
                looked_clean = false;
                if std::time::Instant::now() < deadline {
                    std::thread::sleep(UNWRITTEN_REGISTRATION_POLL);
                    continue;
                }
                let VcsError::CommandFailed { args, repo, stderr } = failure else {
                    unreachable!("only a failed command is waited on")
                };
                return Err(VcsError::CommandFailed {
                    args,
                    repo,
                    stderr: format!(
                        "{stderr}\n{} has been empty for {}s: the `git worktree add` that \
                         was writing it stopped before finishing, and git cannot list this \
                         repository's worktrees until that registration is completed or \
                         removed",
                        unwritten.join(COMMONDIR).display(),
                        wait.as_secs_f64()
                    ),
                });
            }
            let appeared = peers.iter().any(|r| !written_before.contains(r));
            // The writer may have finished between the failure and this
            // look, so a registration written since earns one more attempt;
            // two in a row, with no unfinished registration seen between
            // them, are believed.
            if appeared && !std::mem::replace(&mut looked_clean, true) {
                continue;
            }
            return Err(failure);
        }
    }
}

const COMMONDIR: &str = "commondir";

/// The worktree registrations of one repo, as directories under its common
/// git directory.
struct Registrations {
    root: PathBuf,
}

impl Registrations {
    fn of(dir: &Path) -> Option<Registrations> {
        let common = GitVcs::run_trimmed(
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            dir,
        )
        .ok()?;
        Some(Registrations {
            root: CanonicalPath::of(Path::new(&common))
                .as_path()
                .join("worktrees"),
        })
    }

    fn present(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.root)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    }

    fn written(&self) -> BTreeSet<PathBuf> {
        self.present()
            .into_iter()
            .filter(|r| Self::is_written(r))
            .collect()
    }

    fn is_written(registration: &Path) -> bool {
        std::fs::metadata(registration.join(COMMONDIR)).is_ok_and(|m| m.len() > 0)
    }

    /// Whether `registration` is the one for the worktree at `worktree`: its
    /// `gitdir` names that worktree's `.git`, absolutely or relative to the
    /// registration.
    fn registers(registration: &Path, worktree: &Path) -> bool {
        std::fs::read_to_string(registration.join("gitdir")).is_ok_and(|gitdir| {
            registration
                .join(gitdir.trim_end_matches(['\n', '\r']))
                .parent()
                .is_some_and(|named| same_location(named, worktree))
        })
    }
}

impl GitVcs {
    /// Re-mint a failed `worktree add` as [`VcsError::HookRejected`] when git
    /// left `dest` registered as a worktree.
    ///
    /// git names no hook in its own output: on a `post-checkout` rejection it
    /// writes its progress line and whatever the hook printed, so the only
    /// occurrences of "hook" available to match are the hook author's wording
    /// (absent whenever the hook says "denied by policy") and the destination
    /// path itself (present for any repo under a `hooks/` directory, on
    /// failures no hook was involved in). The registration is git's own
    /// signal instead: it is written before the hook runs, and every failure
    /// that precedes checkout aborts before it exists.
    ///
    /// `registered_before` is what keeps the question about *this* call. A
    /// destination that was already a registered worktree fails the add on the
    /// taken path without any hook running, and its registration is the
    /// previous call's; reading only the after-state calls that a rejection.
    ///
    /// What remains is "did this call get past writing the admin entry", which
    /// a refusing hook satisfies and so would a checkout that dies after that
    /// point. Necessary, not sufficient — see [`VcsError::HookRejected`].
    ///
    /// This runs inside the failing add, so the registration is observed
    /// before any rwv-side rollback can prune it.
    fn is_registered_worktree(&self, store: &Path, dest: &Path) -> bool {
        let Ok(dest) = CanonicalPath::existing(dest) else {
            return false;
        };
        self.list_worktrees(store).is_ok_and(|worktrees| {
            worktrees
                .iter()
                .any(|w| CanonicalPath::existing(w).is_ok_and(|w| w == dest))
        })
    }

    fn classify_worktree_add_failure(
        &self,
        e: VcsError,
        store: &Path,
        dest: &Path,
        registered_before: bool,
    ) -> VcsError {
        let VcsError::CommandFailed { stderr, .. } = &e else {
            return e;
        };
        if registered_before {
            return e;
        }
        if self.is_registered_worktree(store, dest) {
            VcsError::HookRejected {
                repo: store.to_path_buf(),
                stderr: stderr.clone(),
            }
        } else {
            e
        }
    }

    /// Re-mint a failed `merge --ff-only` as [`VcsError::UntrackedCollision`]
    /// when untracked files stand where the advance would have written.
    fn classify_untracked_collision(
        &self,
        e: VcsError,
        repo: &Path,
        to: &ResolvedRevisionId,
    ) -> VcsError {
        if !matches!(e, VcsError::CommandFailed { .. }) {
            return e;
        }
        match self.untracked_paths_in_the_way(repo, to) {
            Some(paths) => VcsError::UntrackedCollision {
                repo: repo.to_path_buf(),
                paths,
            },
            None => e,
        }
    }

    /// The untracked working-tree files an advance to `to` would overwrite:
    /// the paths it newly writes, intersected with what is untracked on disk.
    ///
    /// `None` leaves the caller's raw [`VcsError::CommandFailed`] standing,
    /// which is the answer for every ff-only failure that is not this
    /// condition — a tracked file modified, a directory in the way, a
    /// submodule. Those carry git's own account of themselves; this one
    /// cannot, because the operator needs the path list to act on.
    ///
    /// **The ancestry guard is not redundant with the caller's.** A diverged
    /// tip refuses for a reason no amount of moving files resolves, and its
    /// incoming tree still adds paths that may happen to exist untracked
    /// here. Without the guard those coincidences would be reported as a
    /// collision, sending the operator to clear files and retry into the same
    /// refusal.
    ///
    /// **`--no-renames`** because git's rename detection reports a renamed
    /// file as `R` and would omit the destination the advance still writes.
    fn untracked_paths_in_the_way(
        &self,
        repo: &Path,
        to: &ResolvedRevisionId,
    ) -> Option<Vec<String>> {
        let head = self.head_revision(repo).ok()?;
        if !self.is_ancestor(repo, &head, to).ok()? {
            return None;
        }

        let arriving = Self::run(
            [
                "diff",
                "--name-only",
                "--diff-filter=A",
                "--no-renames",
                "-z",
                head.id().as_str(),
                to.id().as_str(),
            ],
            repo,
        )
        .ok()?;
        let arriving: Vec<&str> = arriving.split('\0').filter(|p| !p.is_empty()).collect();
        if arriving.is_empty() {
            return None;
        }

        let mut args = vec!["ls-files", "--others", "--exclude-standard", "-z", "--"];
        args.extend_from_slice(&arriving);
        let obstructing = Self::run(&args, repo).ok()?;

        let paths: Vec<String> = obstructing
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        (!paths.is_empty()).then_some(paths)
    }

    /// Resolve `rev` to the commit id it names in `repo`, through
    /// `git rev-parse --verify <rev>^{commit}`.
    fn rev_parse(repo: &Path, rev: &str) -> Result<CommitId, VcsError> {
        let deref = format!("{rev}^{{commit}}");
        let args = ["rev-parse", "--verify", &deref];
        commit_id_printed(&Self::run_trimmed(args, repo)?, &args, repo)
    }

    /// Compute the merge-base (common ancestor) of `a` and `b` in `repo`.
    ///
    /// Private helper for [`Vcs::diff_beyond`], which anchors the diff range
    /// at `git merge-base <base> HEAD` rather than the base directly —
    /// diffing against a base that moved on after HEAD forked from it shows
    /// phantom reversals of the work the base gained in the meantime.
    fn merge_base(repo: &Path, a: &CommitId, b: &CommitId) -> Result<CommitId, VcsError> {
        let args = ["merge-base", a.as_str(), b.as_str()];
        commit_id_printed(&Self::run_trimmed(args, repo)?, &args, repo)
    }

    /// List the commits reachable from `to` but not from `from` in `repo`,
    /// newest first, as [`CommitSummary`] records.
    ///
    /// This is `git log <from>..<to>` semantics, so it stays correct when
    /// `from` moved on since `to` forked: the range excludes commits `from`
    /// already has. An empty vec means nothing lies beyond `from`. Private
    /// helper for [`Vcs::commits_beyond`].
    fn log_range(
        repo: &Path,
        from: &CommitId,
        to: &CommitId,
        limit: Option<usize>,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        Self::log_spelled(repo, &format!("{from}..{to}"), limit)
    }

    /// `git log` of `range` as spelled, newest first.
    fn log_spelled(
        repo: &Path,
        range: &str,
        limit: Option<usize>,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        // `%H` full id, `%s` subject — NUL-delimited fields, newline-delimited
        // records, so subjects with spaces/tabs survive. The trailing `--`
        // keeps a range from being read as a path.
        let limit = limit.map(|n| format!("-{n}"));
        let mut args = vec!["log", "--pretty=format:%H%x00%s"];
        args.extend(limit.as_deref());
        args.push(range);
        args.push("--");
        let out = Self::run_trimmed(&args, repo)?;
        out.lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let (id, subject) = line.split_once('\0').unwrap_or((line, ""));
                Ok(CommitSummary {
                    id: commit_id_printed(id, &args, repo)?,
                    subject: subject.to_owned(),
                })
            })
            .collect()
    }

    /// The commit `spelled` names in `repo`, as git resolves a revision.
    ///
    /// Peeling to a commit is what keeps a spelling git would
    /// read as an option from resolving to anything.
    fn endpoint_commit(repo: &Path, spelled: &str) -> Result<CommitId, VcsError> {
        let deref = format!("{spelled}^{{commit}}");
        let args = ["rev-parse", "--verify", "--quiet", &deref];
        match Self::run_trimmed_or_absent(args, repo, Absent::Revision)? {
            Some(printed) => commit_id_printed(&printed, &args, repo),
            None => Err(VcsError::RevisionNotFound {
                repo: repo.to_path_buf(),
                rev: spelled.to_owned(),
            }),
        }
    }

    /// Produce the unified diff of `from..to` in `repo`.
    ///
    /// Private helper for [`Vcs::diff_beyond`], which passes `from` = `git
    /// merge-base <base> HEAD` and `to` = HEAD so the output is what HEAD adds
    /// with no phantom reversals.
    fn diff_range(repo: &Path, from: &CommitId, to: &CommitId) -> Result<String, VcsError> {
        let range = format!("{from}..{to}");
        Self::run_trimmed(["diff", &range], repo)
    }

    /// The directory to run ref-level git commands in for a canonical
    /// store path.
    ///
    /// `store_of` reports the store itself (`<clone>/.git`), not the working
    /// directory. git will accept a `.git` directory as its cwd, but the
    /// behavior differs subtly between porcelain commands, so normalize to
    /// the clone root and leave anything else alone.
    fn work_dir_for_store(store: &Path) -> PathBuf {
        match store.file_name() {
            Some(name) if name == ".git" => store
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| store.to_path_buf()),
            _ => store.to_path_buf(),
        }
    }

    /// The branch name inside a fully-qualified `refs/heads/<name>`.
    ///
    /// The stripping is done here rather than by asking git for a short
    /// name, because git's "short" name is the shortest *unambiguous* one:
    /// with `refs/tags/x` present it renders the branch `x` as `heads/x`,
    /// which is not a branch name and does not round-trip. A symbolic HEAD
    /// pointing outside `refs/heads/` names no branch at all, and is
    /// reported as an error rather than folded into one of the three
    /// attachment states.
    fn local_branch_name_from_full_ref(full: &str, repo: &Path) -> Result<RawRefName, VcsError> {
        match full.strip_prefix(LOCAL_BRANCH_NAMESPACE) {
            Some(name) if !name.is_empty() => Ok(RawRefName::new(name)),
            _ => Err(VcsError::CommandFailed {
                args: vec!["symbolic-ref".to_owned(), "HEAD".to_owned()],
                repo: repo.to_path_buf(),
                stderr: format!("HEAD is symbolic but names no local branch: {full:?}"),
            }),
        }
    }

    /// Detect if a repo is in a mid-operation VCS state (mid-rebase, mid-merge, etc.).
    ///
    /// A bisect counts. It has no conflict-resume path, so it never
    /// appears in [`Vcs::mid_op`] — but it is operator state living in
    /// HEAD's *position*, and repositioning HEAD out from under it loses
    /// the bisect with nothing to resume from. That is the state the
    /// detached-MOVE precondition exists to see.
    pub fn mid_op_state(repo: &Path) -> Option<MidOperation> {
        let git_dir = match Self::run_trimmed(["rev-parse", "--git-dir"], repo) {
            Ok(s) => {
                let p = std::path::PathBuf::from(&s);
                if p.is_absolute() {
                    p
                } else {
                    repo.join(p)
                }
            }
            Err(_) => return None,
        };
        if git_dir.join("rebase-apply").exists() || git_dir.join("rebase-merge").exists() {
            return Some(MidOperation::Conflict(ConflictOp::Rebase));
        }
        if git_dir.join("MERGE_HEAD").exists() {
            return Some(MidOperation::Conflict(ConflictOp::Merge));
        }
        if git_dir.join("CHERRY_PICK_HEAD").exists() {
            return Some(MidOperation::Conflict(ConflictOp::CherryPick));
        }
        if git_dir.join("REVERT_HEAD").exists() {
            return Some(MidOperation::Conflict(ConflictOp::Revert));
        }
        if git_dir.join("BISECT_LOG").exists() {
            return Some(MidOperation::Bisect);
        }
        None
    }

    /// Return a human-readable detail string identifying the commit that
    /// stopped a rebase, for use in conflict-bail messages.
    ///
    /// During a `git rebase` conflict the stopped commit's SHA is written to
    /// `.git/rebase-merge/stopped-sha`. This helper reads that file, resolves
    /// the short SHA via `git rev-parse --short`, and fetches the subject line
    /// via `git log -1 --format=%s`. If any step fails (e.g. the file is
    /// absent or the object is unreachable) it returns a generic fallback so
    /// the caller's conflict message still renders.
    ///
    /// The returned string is suitable as the `detail` arg to
    /// `per_conflict_bail_message` in sync, e.g.:
    /// `"commit 0123456789ab (lock: refresh — post-OOB drift in gc-formulas)"`
    fn rebase_stopped_commit_detail_impl(repo: &Path) -> String {
        let fallback = "see in-flight rebase state for conflicting paths".to_owned();

        // Resolve the .git directory so we can locate rebase-merge/stopped-sha.
        let git_dir = match Self::run_trimmed(["rev-parse", "--git-dir"], repo) {
            Ok(s) => {
                let p = std::path::PathBuf::from(s.trim());
                if p.is_absolute() {
                    p
                } else {
                    repo.join(p)
                }
            }
            Err(_) => return fallback,
        };

        // git may write the stopped commit abbreviated; resolving it yields
        // the id whatever width git chose.
        let stopped_sha_path = git_dir.join("rebase-merge").join("stopped-sha");
        let stopped = match std::fs::read_to_string(&stopped_sha_path)
            .ok()
            .and_then(|s| Self::rev_parse(repo, s.trim()).ok())
        {
            Some(id) => id,
            None => return fallback,
        };
        let short_sha = stopped.short();

        let subject = match Self::run_trimmed(["log", "-1", "--format=%s", stopped.as_str()], repo)
        {
            Ok(s) => s.trim().to_owned(),
            Err(_) => return format!("commit {short_sha}"),
        };
        if subject.is_empty() {
            return format!("commit {short_sha}");
        }

        format!("commit {short_sha} ({subject})")
    }

    /// Return up to `cap` commits of the range `from..to`, newest first,
    /// plus the total commit count in that range.
    ///
    /// Fetches at most `cap + 1` commits, so a separate `git rev-list
    /// --count` runs only when the range is longer than `cap`.
    ///
    /// Returns `(vec![], 0)` on any error so callers can degrade gracefully
    /// when the range is unresolvable (e.g. the object is unreachable in a
    /// shallow clone).
    fn capped_log_range(
        repo: &Path,
        from: &CommitId,
        to: &CommitId,
        cap: usize,
    ) -> (Vec<CommitSummary>, usize) {
        let mut commits = match Self::log_range(repo, from, to, Some(cap + 1)) {
            Ok(c) => c,
            Err(_) => return (vec![], 0),
        };
        if commits.len() <= cap {
            let total = commits.len();
            return (commits, total);
        }
        let range = format!("{from}..{to}");
        let total = Self::run_trimmed(["rev-list", "--count", &range], repo)
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(commits.len());
        commits.truncate(cap);
        (commits, total)
    }

    /// Return `(ahead, behind)` commit counts for `savepoint..tip` and
    /// `tip..savepoint` respectively. Used to determine whether tip is
    /// strictly ahead of savepoint (behind == 0, ahead > 0) or diverged
    /// (both > 0).
    ///
    /// Returns `(0, 0)` on any git error.
    fn ahead_behind_impl(repo: &Path, savepoint: &CommitId, tip: &CommitId) -> (usize, usize) {
        let ahead = Self::run_trimmed(
            ["rev-list", "--count", &format!("{savepoint}..{tip}")],
            repo,
        )
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
        let behind = Self::run_trimmed(
            ["rev-list", "--count", &format!("{tip}..{savepoint}")],
            repo,
        )
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
        (ahead, behind)
    }
}

/// Git-specific "how do I resume this operation?" text for [`ConflictOp`].
///
/// Returned text is a short indented block (no trailing newline) that
/// [`crate::sync`] splices into its conflict-bail messages. The block
/// covers git-vocabulary steps only: edit conflicted files, stage them.
///
/// ## Seam rule and asymmetry by op
///
/// The VCS impl owns git vocabulary (`git add`, `git merge --continue`,
/// `git cherry-pick --continue`). For [`ConflictOp::Merge`] and
/// [`ConflictOp::CherryPick`] the git `--continue` command is the right
/// next step for the operator and is part of this hint.
///
/// [`ConflictOp::Rebase`] is different: rwv has a native
/// `rwv sync --continue` / `rwv land --continue` that covers all
/// remaining replay steps after staging, so the git-level
/// `git rebase --continue` must NOT appear in operator-facing text — rwv
/// core (in `sync.rs`) appends the appropriate `rwv <verb> --continue`
/// line immediately after this hint. The VCS impl deliberately stops at
/// staging for Rebase and lets rwv core own the continue command.
///
/// Callers (rwv core) append the `rwv <verb> --continue` line for Rebase
/// conflicts and `rwv abort` as the final rollback option.
fn git_conflict_resolution_hint(op: ConflictOp) -> String {
    match op {
        ConflictOp::Rebase => {
            // Stop at staging. rwv core appends `rwv sync --continue` /
            // `rwv land --continue` — the VCS impl must not spell rwv
            // vocabulary.
            "  # edit conflicted files\n  git add <files>".to_string()
        }
        ConflictOp::Merge => {
            "  # edit conflicted files\n  git add <files>\n  git merge --continue".to_string()
        }
        ConflictOp::CherryPick => {
            "  # edit conflicted files\n  git add <files>\n  git cherry-pick --continue".to_string()
        }
        ConflictOp::Revert => {
            "  # edit conflicted files\n  git add <files>\n  git revert --continue".to_string()
        }
    }
}

/// The revisions a git range expression names, as spelled: both sides of
/// `A..B` or `A...B` (an empty side is HEAD and names none), or the whole
/// expression less a leading `^`.
fn range_endpoints(range: &str) -> Vec<&str> {
    let sides = match range.split_once("...").or_else(|| range.split_once("..")) {
        Some((left, right)) => vec![left, right],
        None => vec![range.strip_prefix('^').unwrap_or(range)],
    };
    sides.into_iter().filter(|side| !side.is_empty()).collect()
}

/// The exit status by which a git command answers "nothing there". Any other
/// non-zero status is a failure.
#[derive(Clone, Copy)]
enum Absent {
    /// `rev-parse --verify --quiet`: the spelling names no object of the
    /// type asked for.
    Revision,
    /// `config --get` and `config --get-regexp`: no key matches. A key with
    /// no section exits 1 as well, and every key the backend asks for has
    /// one.
    ConfigKey,
    /// `merge-base`, with or without `--all`: the commits share no history.
    MergeBase,
}

impl Absent {
    fn status(self) -> i32 {
        match self {
            Absent::Revision | Absent::ConfigKey | Absent::MergeBase => 1,
        }
    }
}

/// True for transient/internal tags that must not be chosen as a lock's
/// symbolic name. Mirrors the ref-spaces rwv uses for its own bookkeeping —
/// `savepoint/*` (operator/tool savepoints), `rwv/pre-op/*` (sync abort
/// recovery refs under `refs/rwv/pre-op/*` when surfaced as tag names),
/// and `rwv/backup/*` (backups under `refs/rwv/backup/*`).
fn is_transient_tag(tag: &str) -> bool {
    tag.starts_with("savepoint/")
        || tag.starts_with("rwv/pre-op/")
        || tag.starts_with("refs/rwv/pre-op/")
        || tag.starts_with("rwv/backup/")
        || tag.starts_with("refs/rwv/backup/")
        || tag.starts_with("rwv-savepoint/")
}

/// True for release-shape tags (e.g., `v1.2.3`, `v0.3.4-rc1`). Used as a
/// tiebreaker when multiple non-transient tags point at HEAD so a release
/// tag wins over an arbitrary lightweight tag.
///
/// The rule itself lives at the seam
/// ([`crate::vcs::is_release_shape_name`]) because `TrackingRef::parse`
/// asks the same question for the opposite purpose — rejecting a
/// `branch` that names a release rather than a channel. One definition,
/// two callers.
fn is_release_shape_tag(tag: &str) -> bool {
    crate::vcs::is_release_shape_name(tag)
}

impl Vcs for GitVcs {
    fn name(&self) -> &str {
        "git"
    }

    fn minimum_release(&self) -> String {
        let (major, minor) = MINIMUM_RELEASE;
        format!("{major}.{minor}")
    }

    fn tool_readiness(&self) -> ToolReadiness {
        readiness_of(git_reading())
    }

    fn init_repo(&self, dest: &Path) -> Result<(), VcsError> {
        std::fs::create_dir_all(dest).map_err(|e| VcsError::Io {
            ctx: format!("failed to create directory {}", dest.display()),
            source: e,
        })?;
        Self::run(["init", "--initial-branch=main"], dest)?;
        Ok(())
    }

    fn init_bare_repo(&self, dest: &Path) -> Result<(), VcsError> {
        std::fs::create_dir_all(dest).map_err(|e| VcsError::Io {
            ctx: format!("failed to create directory {}", dest.display()),
            source: e,
        })?;
        Self::run(["init", "--bare", "--initial-branch=main"], dest)?;
        Ok(())
    }

    fn clone_repo(&self, url: &CloneUrl, dest: &Path) -> Result<RemoteHandle, VcsError> {
        let dest_arg = GitArg::from_path(dest, "destination path")?;
        Self::run(
            Argv::from(["clone"]).text(url.as_str()).path(&dest_arg),
            Path::new("."),
        )?;
        Self::only_remote(dest)
    }

    fn fetch_remote(&self, repo: &Path, remote: &RemoteHandle) -> Result<(), VcsError> {
        Self::run(["fetch", "--tags", remote.name()], repo)?;
        Ok(())
    }

    fn upstream(&self, on: &AttachedRef) -> Result<Option<Upstream>, VcsError> {
        self.verify_attachment(on)?;
        let branch = on.to_string();
        let remote = Self::config_get(on.repo(), &format!("branch.{branch}.remote"))?;
        let merge = Self::config_get(on.repo(), &format!("branch.{branch}.merge"))?;
        Ok(match (remote, merge) {
            (Some(remote), Some(merge)) if remote != "." => merge
                .strip_prefix(LOCAL_BRANCH_NAMESPACE)
                .map(|upstream| Upstream {
                    remote: RemoteHandle::new(remote),
                    branch: RawRefName::new(upstream),
                }),
            _ => None,
        })
    }

    fn push_target(
        &self,
        on: &AttachedRef,
        url_remote: Option<&RemoteHandle>,
    ) -> Result<Option<RemoteHandle>, VcsError> {
        self.verify_attachment(on)?;
        self.push_target_of(on.repo(), &RawRefName::new(on.to_string()), url_remote)
    }

    fn push_target_of(
        &self,
        repo: &Path,
        branch: &RawRefName,
        url_remote: Option<&RemoteHandle>,
    ) -> Result<Option<RemoteHandle>, VcsError> {
        for key in [
            format!("branch.{branch}.pushRemote"),
            "remote.pushDefault".to_owned(),
            format!("branch.{branch}.remote"),
        ] {
            if let Some(name) = Self::config_get(repo, &key)? {
                if name != "." {
                    return Ok(Some(RemoteHandle::new(name)));
                }
            }
        }
        Ok(url_remote.cloned())
    }

    fn own_push_remote(
        &self,
        repo: &Path,
        branch: &RawRefName,
    ) -> Result<Option<RemoteHandle>, VcsError> {
        for key in [
            format!("branch.{branch}.pushRemote"),
            "remote.pushDefault".to_owned(),
        ] {
            if let Some(name) = Self::config_get(repo, &key)? {
                return Ok(Some(RemoteHandle::new(name)));
            }
        }
        Ok(None)
    }

    fn push_branch(
        &self,
        on: &AttachedRef,
        target: &RemoteHandle,
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        self.verify_attachment(on)?;
        let tracking = remote_tracking(target, &on.to_string());
        let args = [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{tracking}^{{commit}}"),
        ];
        let output = git_command()
            .args(args)
            .current_dir(on.repo())
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git rev-parse in {}", on.repo().display()),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let printed = String::from_utf8_lossy(&output.stdout);
        Ok(Some(ResolvedRevisionId::from_canonical(
            commit_id_printed(printed.trim(), &args, on.repo())?,
            None,
        )))
    }

    fn home_remote(
        &self,
        repo: &Path,
        url_remote: &RemoteHandle,
    ) -> Result<RemoteHandle, VcsError> {
        if let Some(name) = Self::config_get(repo, "checkout.defaultRemote")? {
            if Self::remote_names(repo)?.contains(&name) {
                return Ok(RemoteHandle::new(name));
            }
        }
        Ok(url_remote.clone())
    }

    fn url_remote(&self, repo: &Path, url: &CloneUrl) -> Result<UrlRemote, VcsError> {
        let recorded = self.recorded_urls(repo)?;
        for r in &recorded {
            if let RecordedRemote::Url(have) = RecordedRemote::read(r.url.clone()) {
                if have.names_same_remote(url) {
                    return Ok(UrlRemote::Present(r.remote.clone()));
                }
            }
        }
        let name = if recorded.is_empty() {
            GIT_FIRST_REMOTE_NAME.to_owned()
        } else {
            owner_segment(url.as_str()).unwrap_or_else(|| GIT_FIRST_REMOTE_NAME.to_owned())
        };
        let remote = RemoteHandle::new(name);
        Ok(match recorded.iter().find(|r| r.remote == remote) {
            Some(holder) => UrlRemote::NameTaken(RemoteNameTaken {
                remote,
                holds: holder.url.clone(),
                wanted: url.clone(),
            }),
            None => UrlRemote::Absent(AbsentRemote::new(remote, url.clone())),
        })
    }

    fn add_url_remote(&self, repo: &Path, absent: &AbsentRemote) -> Result<RemoteHandle, VcsError> {
        Self::run(
            [
                "remote",
                "add",
                absent.remote().name(),
                absent.url().as_str(),
            ],
            repo,
        )?;
        Ok(absent.remote().clone())
    }

    fn project_url_remote(&self, repo: &Path) -> Result<Option<RemoteHandle>, VcsError> {
        if !self.is_repo(repo) {
            return Err(VcsError::NotARepo(repo.to_path_buf()));
        }
        let names = Self::remote_names(repo)?;
        if let Some(recorded) = Self::config_get(repo, PROJECT_URL_REMOTE_KEY)? {
            if names.contains(&recorded) {
                return Ok(Some(RemoteHandle::new(recorded)));
            }
        }
        Ok(match names.as_slice() {
            [only] => Some(RemoteHandle::new(only.clone())),
            _ => None,
        })
    }

    fn record_project_url_remote(
        &self,
        repo: &Path,
        remote: &RemoteHandle,
    ) -> Result<(), VcsError> {
        Self::run(
            ["config", "--local", PROJECT_URL_REMOTE_KEY, remote.name()],
            repo,
        )?;
        Ok(())
    }

    fn recorded_urls(&self, repo: &Path) -> Result<Vec<RecordedUrl>, VcsError> {
        let Some(listing) = Self::run_or_absent(
            ["config", "-z", "--get-regexp", r"^remote\..*\.url$"],
            repo,
            Absent::ConfigKey,
        )?
        else {
            return Ok(Vec::new());
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for record in listing.split_terminator('\0') {
            let (key, url) = record.split_once('\n').unwrap_or((record, ""));
            let Some(name) = key
                .strip_prefix("remote.")
                .and_then(|rest| rest.strip_suffix(".url"))
            else {
                continue;
            };
            if seen.insert(name.to_owned()) {
                out.push(RecordedUrl {
                    remote: RemoteHandle::new(name),
                    url: url.to_owned(),
                });
            }
        }
        Ok(out)
    }

    fn reached_by_remote(
        &self,
        repo: &Path,
        commit: &ResolvedRevisionId,
        remote: &RemoteHandle,
    ) -> Result<bool, VcsError> {
        let namespace = remote_tracking_namespace(remote);
        let listing = Self::run_trimmed(
            [
                "for-each-ref",
                "--contains",
                commit.id().as_str(),
                "--format=%(refname)",
                namespace.as_str(),
            ],
            repo,
        )?;
        Ok(listing
            .lines()
            .any(|r| !r.trim().is_empty() && !r.ends_with("/HEAD")))
    }

    fn upstream_tip(&self, on: &AttachedRef) -> Result<Option<ResolvedRevisionId>, VcsError> {
        self.verify_attachment(on)?;
        let args = ["rev-parse", "--verify", "--quiet", "@{upstream}^{commit}"];
        let output = git_command()
            .args(args)
            .current_dir(on.repo())
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git rev-parse in {}", on.repo().display()),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let printed = String::from_utf8_lossy(&output.stdout);
        Ok(Some(ResolvedRevisionId::from_canonical(
            commit_id_printed(printed.trim(), &args, on.repo())?,
            Some("upstream".to_owned()),
        )))
    }

    fn fetch_pruning(
        &self,
        _claim: &crate::fetch_claim::FetchClaim,
        repo: &Path,
        only: Option<&[RemoteHandle]>,
    ) -> Result<PruningFetch, VcsError> {
        let repo = Self::work_dir_for_store(repo);
        let repo = repo.as_path();
        let before = Self::remote_tracking_tips(repo)?;
        let mut log = String::new();
        let mut unconfirmed = Vec::new();
        let mut fetched = Vec::new();
        let remotes = Self::run_trimmed(["remote"], repo)?;
        let wanted = |remote: &str| only.is_none_or(|only| only.iter().any(|r| r.name() == remote));
        for remote in remotes
            .lines()
            .map(str::trim)
            .filter(|r| !r.is_empty() && wanted(r))
        {
            match Self::run_logged(["fetch", "--tags", "--prune", remote], repo) {
                Ok(report) => {
                    log.push_str(&report);
                    fetched.push(RemoteHandle::new(remote));
                }
                Err(VcsError::CommandFailed { stderr, .. }) => {
                    unconfirmed.push(UnconfirmedRemote {
                        remote: remote.to_owned(),
                        cause: stderr
                            .trim()
                            .lines()
                            .next()
                            .map_or("fetch failed", str::trim_end)
                            .to_owned(),
                    });
                }
                Err(e) => return Err(e),
            }
        }
        let after = Self::remote_tracking_tips(repo)?;
        let mut moves = Vec::new();
        for (branch, to) in &after {
            let change = match before.get(branch) {
                None => BranchChange::Created { to: to.clone() },
                Some(from) if from == to => continue,
                Some(from) if Self::descends(repo, from, to)? => BranchChange::Advanced {
                    from: from.clone(),
                    to: to.clone(),
                },
                Some(from) => {
                    Self::record_superseded_tip(repo, branch, from)?;
                    BranchChange::Rewritten {
                        from: from.clone(),
                        to: to.clone(),
                    }
                }
            };
            moves.push(RemoteBranchMove {
                branch: branch.clone(),
                change,
            });
        }
        for (branch, from) in &before {
            if !after.contains_key(branch) {
                moves.push(RemoteBranchMove {
                    branch: branch.clone(),
                    change: BranchChange::Pruned { from: from.clone() },
                });
            }
        }
        moves.sort_by(|a, b| a.branch.cmp(&b.branch));
        Ok(PruningFetch {
            moves,
            fetched,
            log,
            unconfirmed,
        })
    }

    fn resolve_branch_on_remote(
        &self,
        repo: &Path,
        remote: &RemoteHandle,
        branch: &RemoteRef,
    ) -> Result<ResolvedRevisionId, VcsError> {
        let branch = branch.branch();
        let tip = self.resolve_revision(repo, &remote_tracking(remote, branch))?;
        Ok(ResolvedRevisionId::from_canonical(
            tip.id().clone(),
            Some(format!("{}/{branch}", remote.name())),
        ))
    }

    fn head_revision(&self, repo: &Path) -> Result<ResolvedRevisionId, VcsError> {
        let args = ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"];
        match Self::run_trimmed_or_absent(args, repo, Absent::Revision)? {
            None => {
                // git cannot resolve HEAD. Whether that is because the repo
                // has no commits yet is a question about HEAD's state, so it
                // is asked where such questions are answered — this arm only
                // renders the result. (`head_attachment` distinguishes unborn
                // from detached from broken; anything other than unborn
                // leaves the hint unnamed, exactly as before.)
                let branch_hint = match self.head_attachment(repo) {
                    Ok(HeadAttachment::Unborn(u)) => u.name().to_string(),
                    _ => "(unknown)".to_owned(),
                };
                Err(VcsError::CommandFailed {
                    args: args.map(str::to_owned).to_vec(),
                    repo: repo.to_path_buf(),
                    stderr: format!(
                        "unborn HEAD (no commits yet, on branch '{branch_hint}'): \
                         make an initial commit, then pin it with `rwv member pin`"
                    ),
                })
            }
            Some(sha) => {
                // A tag at HEAD becomes the display form, so human-readable output
                // can name it (e.g. `v0.3.4`) without an extra resolve step. It is
                // never what a lock records.
                let display = self.tag_at_head(repo)?.map(|t| t.as_str().to_string());
                let id = commit_id_printed(&sha, &args, repo)?;
                Ok(ResolvedRevisionId::from_canonical(id, display))
            }
        }
    }

    fn resolve_revision(&self, repo: &Path, rev: &str) -> Result<ResolvedRevisionId, VcsError> {
        let deref = format!("{rev}^{{commit}}");
        let args = ["rev-parse", "--verify", "--quiet", &deref];
        match Self::run_trimmed_or_absent(args, repo, Absent::Revision)? {
            Some(canonical) => Ok(ResolvedRevisionId::from_canonical(
                commit_id_printed(&canonical, &args, repo)?,
                Some(rev.to_string()),
            )),
            None => Err(VcsError::RevisionNotFound {
                repo: repo.to_path_buf(),
                rev: rev.to_string(),
            }),
        }
    }

    fn remove_worktree(&self, repo: &Path, worktree_path: &Path) -> Result<(), VcsError> {
        let worktree = GitArg::from_path(worktree_path, "worktree path")?;
        Self::run_awaiting_peers(
            Argv::from(["worktree", "remove", "--force"]).path(&worktree),
            repo,
        )?;
        Ok(())
    }

    fn is_repo(&self, path: &Path) -> bool {
        // git skips an invalid `.git` entry and keeps walking up, so a
        // successful `rev-parse` alone does not mean `path` is the repo: the
        // top level it reports must be `path` itself.
        if !path.join(".git").exists() {
            return false;
        }
        let Ok(top) = Self::run_trimmed(["rev-parse", "--show-toplevel"], path) else {
            return false;
        };
        match (
            CanonicalPath::existing(Path::new(&top)),
            CanonicalPath::existing(path),
        ) {
            (Ok(top), Ok(path)) => top == path,
            _ => false,
        }
    }

    fn list_worktrees(&self, repo: &Path) -> Result<Vec<PathBuf>, VcsError> {
        let output = Self::run_trimmed_awaiting_peers(["worktree", "list", "--porcelain"], repo)?;
        let paths = output
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .collect();
        Ok(paths)
    }

    fn has_uncommitted_changes(&self, repo: &Path) -> Result<bool, VcsError> {
        // One record per dirty entry, untracked included; none means clean.
        Ok(!Self::porcelain_status(repo, StatusReach::Untracked)?.is_empty())
    }

    fn dirty_file_names(&self, repo: &Path) -> Result<Vec<String>, VcsError> {
        Self::dirty_file_names_inner(repo, StatusReach::Untracked)
    }

    fn tracked_dirty_file_names(&self, repo: &Path) -> Result<Vec<String>, VcsError> {
        Self::dirty_file_names_inner(repo, StatusReach::Tracked)
    }

    fn ignored_file_names(&self, repo: &Path) -> Result<Vec<String>, VcsError> {
        Ok(Self::porcelain_status(repo, StatusReach::Ignored)?
            .into_iter()
            .filter(|entry| entry.staged == b'!')
            .map(|entry| entry.path)
            .collect())
    }

    fn ignored_removal_hint(&self, workspace: &Path) -> String {
        format!(
            "`git -C {at} clean -X -d -n` lists what would go, and \
             `git -C {at} clean -X -d -f` deletes it",
            at = workspace.display(),
        )
    }
    fn stage_paths(&self, repo: &Path, paths: &[&str]) -> Result<(), VcsError> {
        let mut args: Vec<&str> = vec!["add", "--"];
        args.extend_from_slice(paths);
        Self::run(&args, repo).map(|_| ())
    }
    fn stage_content(&self, repo: &Path, path: &Path, content: &[u8]) -> Result<(), VcsError> {
        let path = GitArg::from_path(path, "path")?;
        let blob = Self::run_with_stdin(["hash-object", "-w", "--stdin"], repo, content)?;
        let entry = format!("100644,{},{}", blob.trim(), path.as_str());
        Self::run(
            ["update-index", "--add", "--cacheinfo", entry.as_str()],
            repo,
        )
        .map(|_| ())
    }
    fn unstage_paths(&self, repo: &Path, paths: &[&str]) -> Result<(), VcsError> {
        let mut args: Vec<&str> = vec!["reset", "-q", "--"];
        args.extend_from_slice(paths);
        Self::run(&args, repo).map(|_| ())
    }
    fn discard_captured_change(
        &self,
        repo: &Path,
        path: &Path,
        captured: &[u8],
    ) -> Result<DiscardOutcome, VcsError> {
        let arg = GitArg::from_path(path, "path")?;
        let file = repo.join(path);
        if std::fs::read(&file).ok().as_deref() != Some(captured) {
            return Ok(DiscardOutcome::ChangedSinceCapture);
        }
        let columns: Vec<(u8, u8)> = Self::porcelain_status(repo, StatusReach::Untracked)?
            .into_iter()
            .filter(|entry| {
                entry.path == arg.as_str() || entry.renamed_from.as_deref() == Some(arg.as_str())
            })
            .map(|entry| (entry.staged, entry.worktree))
            .collect();
        let delete_working_file = || {
            std::fs::remove_file(&file).map_err(|e| VcsError::Io {
                ctx: format!("failed to remove {}", file.display()),
                source: e,
            })
        };
        match columns.as_slice() {
            [] => {}
            [(b' ', b'M')] | [(b'M', b' ')] => {
                Self::run(Argv::from(["reset", "-q", "--"]).path(&arg), repo)?;
                Self::run(
                    Argv::from(["checkout-index", "-f", "-u", "--"]).path(&arg),
                    repo,
                )?;
            }
            [(b'A', b' ')] => {
                Self::run(Argv::from(["reset", "-q", "--"]).path(&arg), repo)?;
                delete_working_file()?;
            }
            [(b'?', b'?')] => delete_working_file()?,
            _ => return Ok(DiscardOutcome::HoldsUncapturedChange),
        }
        Ok(DiscardOutcome::Discarded)
    }
    fn has_staged_changes(&self, repo: &Path) -> Result<bool, VcsError> {
        // Exit 0 means no difference between index and HEAD, so a non-zero
        // exit is the positive answer and not a failure to report.
        let out = git_command()
            .args(["diff", "--cached", "--quiet"])
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: "failed to spawn git diff --cached --quiet".to_owned(),
                source: e,
            })?;
        Ok(!out.status.success())
    }
    fn staged_paths(&self, repo: &Path) -> Result<Vec<String>, VcsError> {
        Ok(Self::porcelain_status(repo, StatusReach::Tracked)?
            .into_iter()
            .filter(|entry| entry.staged != b' ' && entry.staged != b'?')
            .flat_map(|entry| std::iter::once(entry.path).chain(entry.renamed_from))
            .collect())
    }
    fn unstaged_paths(&self, repo: &Path) -> Result<Vec<String>, VcsError> {
        Ok(Self::porcelain_status(repo, StatusReach::Tracked)?
            .into_iter()
            .filter(|entry| entry.worktree != b' ' && entry.worktree != b'?')
            .map(|entry| entry.path)
            .collect())
    }
    fn commit(&self, repo: &Path, message: &str) -> Result<(), VcsError> {
        Self::run(["commit", "-m", message], repo).map(|_| ())
    }
    fn commit_only(
        &self,
        repo: &Path,
        paths: &[&str],
        message: &str,
        annotations: &[Annotation],
    ) -> Result<bool, VcsError> {
        let message = with_trailers(message, annotations);
        let index = Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &format!("rwv-commit-index-{}", std::process::id()),
            ],
            repo,
        )?;
        let born = Self::run_or_absent(
            ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            repo,
            Absent::Revision,
        )?
        .is_some();
        let outcome = Self::commit_through_own_index(repo, &index, born, paths, &message);
        let _ = std::fs::remove_file(&index);
        outcome
    }
    fn has_commit_identity(&self, dir: &Path) -> Result<bool, VcsError> {
        Self::has_identity(dir)
    }
    fn annotation_values(
        &self,
        repo: &Path,
        commit: &CommitId,
        key: &str,
    ) -> Result<Vec<String>, VcsError> {
        let format = format!("--format=%(trailers:key={key},valueonly,unfold)");
        Ok(
            Self::run(["log", "-1", &format, commit.as_str(), "--"], repo)?
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }
    fn is_tracked(&self, repo: &Path, path: &Path) -> Result<bool, VcsError> {
        let path = GitArg::from_path(path, "path")?;
        match Self::run(
            Argv::from(["ls-files", "--error-unmatch"]).path(&path),
            repo,
        ) {
            Ok(_) => Ok(true),
            // `--error-unmatch` exits non-zero for every "no" this question
            // has: untracked, ignored, outside the repo, not a repo at all.
            Err(VcsError::CommandFailed { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn holds_committed_content(&self, repo: &Path, path: &Path) -> Result<bool, VcsError> {
        let is_file =
            std::fs::symlink_metadata(repo.join(path)).is_ok_and(|meta| meta.file_type().is_file());
        if !is_file
            || !self.is_repo(repo)
            || matches!(self.head_attachment(repo)?, HeadAttachment::Unborn(_))
        {
            return Ok(false);
        }
        let path = GitArg::from_path(path, "path")?;
        let listing = Self::run(
            Argv::from(["--literal-pathspecs", "ls-tree", "-z", "HEAD", "--"]).path(&path),
            repo,
        )?;
        let committed_blob = listing.split('\0').find_map(|record| {
            let (meta, name) = record.split_once('\t')?;
            let mut fields = meta.split(' ');
            let (mode, kind, oid) = (fields.next()?, fields.next()?, fields.next()?);
            (name == path.as_str()
                && kind == "blob"
                && FileMode::parse(mode).is_some_and(FileMode::is_regular_file))
            .then_some(oid)
        });
        Ok(match committed_blob {
            Some(blob) => Self::unfiltered_hashes(repo, &[path.as_str()])?
                .first()
                .is_some_and(|hash| hash == blob),
            None => false,
        })
    }

    fn tag_at_head(&self, repo: &Path) -> Result<Option<RawRefName>, VcsError> {
        // `git tag --points-at HEAD` lists tags that resolve to HEAD.
        //
        // Filter out transient/internal tags (savepoints and pre-op refs) so
        // they're never chosen as the symbolic name when writing a lock. If
        // only transient tags point at HEAD, we return `None` so callers fall
        // back to the canonical SHA.
        //
        // Among remaining tags, prefer release-shape tags (e.g., `v1.2.3`)
        // over arbitrary lightweight tags, so a workspace with both
        // `v9.9.9` and `tmp-foo` writes `v9.9.9`.
        let output = Self::run_trimmed(["tag", "--points-at", "HEAD"], repo)?;
        let candidates: Vec<&str> = output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|t| !is_transient_tag(t))
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        // Prefer a release-shape tag; otherwise fall back to the first.
        let chosen = candidates
            .iter()
            .find(|t| is_release_shape_tag(t))
            .copied()
            .unwrap_or(candidates[0]);
        Ok(Some(RawRefName::new(chosen)))
    }

    fn worktree_prune(&self, repo: &Path) -> Result<(), VcsError> {
        Self::run(["worktree", "prune"], repo)?;
        Ok(())
    }

    fn conflict_resolution_hint(&self, op: ConflictOp) -> String {
        git_conflict_resolution_hint(op)
    }

    fn hint(&self, action: VcsAction) -> &'static str {
        match action {
            VcsAction::Commit => "git -C <checkout> commit",
            VcsAction::Stash => "git -C <checkout> stash push",
            VcsAction::Status => "git -C <checkout> status",
            VcsAction::RemoveWorktree => "git -C <checkout> worktree remove <worktree>",
            VcsAction::BranchAt => "git -C <checkout> branch <name> <sha>",
            VcsAction::SwitchTo => "git -C <checkout> switch <E>",
            VcsAction::SwitchDetached => "git -C <checkout> switch --detach <r>/<b>",
            VcsAction::CherryPick => "git -C <checkout> cherry-pick <ids>",
            VcsAction::CherryPickOntoCheckedOut => "git -C <b-checkout> cherry-pick <ids>",
            VcsAction::CherryPickMainline => "git -C <checkout> cherry-pick -m 1 <ids>",
            VcsAction::Revert => "git -C <checkout> revert <ids>",
            VcsAction::MergeNoFastForward => "git -C <checkout> merge --no-ff <pin>",
            VcsAction::PushCommitAs => "git -C <checkout> push <r> <sha>:refs/heads/<name>",
            VcsAction::SetPushRemote => "git -C <checkout> config branch.<b>.pushRemote <r>",
            VcsAction::SetPushDefault => "git -C <checkout> config remote.pushDefault <r>",
            VcsAction::SetDefaultRemote => "git -C <checkout> config checkout.defaultRemote <r>",
            VcsAction::AddRemote => "git -C <checkout> remote add <r> <url>",
            VcsAction::UnsetUpstream => "git -C <checkout> branch --unset-upstream",
            VcsAction::SetCommitIdentity => {
                "git config --global user.name <name> && git config --global user.email <email>"
            }
        }
    }

    fn rebase(
        &self,
        repo: &Path,
        onto: &ResolvedRevisionId,
        upstream: &ResolvedRevisionId,
        derived: DerivedContentPolicy,
    ) -> Result<(), VcsError> {
        // `git rebase --onto <onto> <upstream>` replays commits in
        // <upstream>..HEAD onto <onto>. On conflict, git leaves the repo
        // mid-rebase (rebase-merge/ + conflict markers in WT). We detect that
        // state and surface VcsError::RebaseConflict so the caller can pair
        // with conflict_resolution_hint(ConflictOp::Rebase).
        //
        // `--empty=drop`: drop commits that become empty after rebase. This
        // is what turns a derived-content-only commit into a silent no-op
        // when the policy resolved its paths to the target side, leaving
        // nothing for the commit to record.
        //
        // `--no-keep-empty`: also drop commits that were originally empty
        // (e.g. `git commit --allow-empty`), so regeneration noise does not
        // survive a replay.
        //
        // `--force-rebase`: force a replay even when `upstream` is already an
        // ancestor of HEAD. Without it, git short-circuits to "up to date" —
        // and the commits that should have been dropped survive.
        let rebase_args = [
            "rebase",
            "--force-rebase",
            "--no-keep-empty",
            "--empty=drop",
            "--onto",
            onto.id().as_str(),
            upstream.id().as_str(),
        ];
        let mut args = derived_content_git_args(derived);
        args.extend(rebase_args.iter().map(|a| (*a).to_owned()));

        let output = git_command()
            .args(&args)
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!(
                    "failed to spawn git rebase --onto {} {}",
                    onto.id().as_str(),
                    upstream.id().as_str()
                ),
                source: e,
            })?;

        if output.status.success() {
            return Ok(());
        }

        // Non-zero exit. If the repo is in mid-rebase, this is a conflict;
        // otherwise it's some other rebase error (bad refs, etc.).
        if Self::mid_op_state(repo) == Some(MidOperation::Conflict(ConflictOp::Rebase)) {
            return Err(VcsError::RebaseConflict {
                repo: repo.to_path_buf(),
                op: ConflictOp::Rebase,
            });
        }

        // Report the operation, not the policy: the `-c` pairs are how the
        // policy is spelled to git, not something the operator asked for.
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(VcsError::CommandFailed {
            args: rebase_args.iter().map(|a| (*a).to_owned()).collect(),
            repo: repo.to_path_buf(),
            stderr,
        })
    }

    fn rebase_continue(&self, repo: &Path, derived: DerivedContentPolicy) -> Result<(), VcsError> {
        // Caller contract: `repo` must be mid-rebase. Enforce it here — the
        // call site already inspects `mid_op` to route between a fresh
        // rebase and this method, so reaching this function on a clean repo
        // is a bug, not an in-band condition.
        if Self::mid_op_state(repo) != Some(MidOperation::Conflict(ConflictOp::Rebase)) {
            return Err(VcsError::CommandFailed {
                args: vec!["rebase".to_owned(), "--continue".to_owned()],
                repo: repo.to_path_buf(),
                stderr: format!(
                    "rebase_continue called on {} which is not {}",
                    repo.display(),
                    MidOperation::Conflict(ConflictOp::Rebase)
                ),
            });
        }

        let mut args = derived_content_git_args(derived);
        args.push("rebase".to_owned());
        args.push("--continue".to_owned());

        // `git rebase --continue` invokes `$EDITOR` on the stopped commit's
        // message before recording it. rwv is non-interactive, so pin both
        // editor hooks to the `true` command. Env is scoped to this single
        // subprocess; the operator's own `git rebase --continue` outside rwv
        // is unaffected.
        let output = git_command()
            .args(&args)
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!(
                    "failed to spawn git rebase --continue in {}",
                    repo.display()
                ),
                source: e,
            })?;

        if output.status.success() {
            return Ok(());
        }

        // Non-zero exit. If the repo is STILL mid-rebase, git either stopped
        // on a further genuine conflict OR the operator's resolution left
        // conflict markers unstaged. Both surface as the same operator
        // signal: resolve → stage → resume.
        if Self::mid_op_state(repo) == Some(MidOperation::Conflict(ConflictOp::Rebase)) {
            return Err(VcsError::RebaseConflict {
                repo: repo.to_path_buf(),
                op: ConflictOp::Rebase,
            });
        }

        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(VcsError::CommandFailed {
            args: vec!["rebase".to_owned(), "--continue".to_owned()],
            repo: repo.to_path_buf(),
            stderr,
        })
    }

    fn derived_content_dropped_by_replay(
        &self,
        repo: &Path,
        base: &ResolvedRevisionId,
        source: &ResolvedRevisionId,
        landed: &ResolvedRevisionId,
    ) -> Result<Vec<String>, VcsError> {
        let fork_point_to_source = format!("{}...{}", base.id().as_str(), source.id().as_str());
        let changed_by_range = nul_terminated_fields(&Self::run(
            ["diff", "--name-only", "-z", &fork_point_to_source],
            repo,
        )?);
        if changed_by_range.is_empty() {
            return Ok(Vec::new());
        }
        let not_reproduced: std::collections::BTreeSet<String> = nul_terminated_fields(&Self::run(
            [
                "diff",
                "--name-only",
                "-z",
                landed.id().as_str(),
                source.id().as_str(),
            ],
            repo,
        )?)
        .into_iter()
        .collect();

        let candidates: Vec<String> = changed_by_range
            .into_iter()
            .filter(|p| not_reproduced.contains(p))
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut args: Vec<&str> = vec!["check-attr", "-z", "merge", "--"];
        args.extend(candidates.iter().map(String::as_str));
        let attrs = Self::run(&args, repo)?;

        // `check-attr -z` emits a flat NUL-separated stream of
        // (path, attribute, value) triples, one per queried path.
        let fields = nul_terminated_fields(&attrs);
        Ok(fields
            .chunks_exact(3)
            .filter(|triple| {
                triple[2] == DERIVED_CONTENT_DRIVER_NAME || triple[2] == RWV_MERGE_DRIVER_NAME
            })
            .map(|triple| triple[0].clone())
            .collect())
    }

    fn changed_paths_between(
        &self,
        repo: &Path,
        from: &ResolvedRevisionId,
        to: &ResolvedRevisionId,
    ) -> Result<Vec<String>, VcsError> {
        Ok(nul_terminated_fields(&Self::run(
            [
                "diff",
                "--name-only",
                "-z",
                from.id().as_str(),
                to.id().as_str(),
            ],
            repo,
        )?))
    }

    fn lock_driver_setup(&self, repo: &Path) -> Result<LockDriverSetup, VcsError> {
        if !self.is_repo(repo) {
            return Err(VcsError::NotARepo(repo.to_path_buf()));
        }
        let born = Self::run_or_absent(
            ["rev-parse", "--verify", "-q", "HEAD^{commit}"],
            repo,
            Absent::Revision,
        )?
        .is_some();
        let declaring = if born {
            let listed = Self::run_trimmed(
                ["ls-tree", "--name-only", "HEAD", "--", ATTRIBUTES_FILE],
                repo,
            )?;
            if listed.is_empty() {
                None
            } else {
                Some(Self::run(
                    ["show", &format!("HEAD:{ATTRIBUTES_FILE}")],
                    repo,
                )?)
            }
        } else {
            match std::fs::read_to_string(repo.join(ATTRIBUTES_FILE)) {
                Ok(text) => Some(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(VcsError::Io {
                        ctx: format!("failed to read {}", repo.join(ATTRIBUTES_FILE).display()),
                        source: e,
                    })
                }
            }
        };
        Ok(LockDriverSetup {
            declaration: lock_attribute_in(declaring.as_deref()),
            definition: lock_driver_config_part(repo)?,
        })
    }

    fn lock_driver_declaration_file(&self) -> &'static str {
        ATTRIBUTES_FILE
    }

    fn declare_lock_driver_in(&self, text: Option<&str>) -> Option<String> {
        declare_lock_driver_in(text)
    }

    fn define_lock_driver(&self, repo: &Path) -> Result<(), VcsError> {
        Self::run(
            [
                "config",
                RWV_MERGE_DRIVER_CONFIG_KEY,
                RWV_MERGE_DRIVER_COMMAND,
            ],
            repo,
        )?;
        Self::run(
            [
                "config",
                RWV_MERGE_DRIVER_NAME_KEY,
                RWV_MERGE_DRIVER_NAME_DESC,
            ],
            repo,
        )?;
        for key in SUPERSEDED_DRIVER_KEYS {
            if config_value(repo, Some("--local"), key)?.is_some() {
                Self::run(["config", "--local", "--unset-all", key], repo)?;
            }
        }
        Ok(())
    }

    fn merging_commits(&self, repo: &Path) -> Option<(CommitId, CommitId)> {
        merging_heads(repo)
    }

    fn advance_if_fast_forward(
        &self,
        repo: &Path,
        to: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        let merge_args = ["merge", "--ff-only", to.id().as_str()];
        let output = git_command()
            .args(["-c", "advice.diverging=false"])
            .args(merge_args)
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {merge_args:?}"),
                source: e,
            })?;

        if output.status.success() {
            return Ok(());
        }

        // Report the operation, not the flag: `-c advice.diverging=false`
        // is how this call silences git's own hint, not something the
        // operator asked for.
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let err = VcsError::CommandFailed {
            args: merge_args.iter().map(|a| (*a).to_owned()).collect(),
            repo: repo.to_path_buf(),
            stderr,
        };
        Err(self.classify_untracked_collision(err, repo, to))
    }

    fn hard_reset(&self, repo: &Path, to: &ResolvedRevisionId) -> Result<(), VcsError> {
        Self::run(["reset", "--hard", to.id().as_str()], repo)?;
        Ok(())
    }

    fn is_ancestor(
        &self,
        repo: &Path,
        ancestor: &ResolvedRevisionId,
        descendant: &ResolvedRevisionId,
    ) -> Result<bool, VcsError> {
        let status = git_command()
            .args([
                "merge-base",
                "--is-ancestor",
                ancestor.id().as_str(),
                descendant.id().as_str(),
            ])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git merge-base in {}", repo.display()),
                source: e,
            })?;
        // Exit 0 = is ancestor; exit 1 = is not. Other exits indicate a
        // problem (e.g. unknown revision); collapse them into "not an
        // ancestor" to match the existing sync.rs semantics — callers
        // treat the false case as a refusal and fall back accordingly.
        Ok(status.success())
    }

    fn count_commits_in_range(
        &self,
        repo: &Path,
        from: &ResolvedRevisionId,
        to: &ResolvedRevisionId,
    ) -> Result<usize, VcsError> {
        let range = format!("{}..{}", from.id().as_str(), to.id().as_str());
        let out = Self::run_trimmed(["rev-list", "--count", &range], repo)?;
        Ok(out.trim().parse::<usize>().unwrap_or(0))
    }

    fn create_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
    ) -> Result<ResolvedRevisionId, VcsError> {
        let head = self.head_revision(repo)?;
        let saved = match self.observe_head(repo)? {
            HeadObservation::Attached { name } => SavedCheckout::On(name),
            HeadObservation::Detached { .. } | HeadObservation::Unborn { .. } => {
                SavedCheckout::Detached
            }
        };
        self.drop_checkout_savepoint(repo, key)?;
        Self::run(
            ["update-ref", &saved.ref_name(key), head.id().as_str()],
            repo,
        )?;
        Ok(head)
    }

    fn create_ref_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        branch: &RawRefName,
        at: &CommitId,
    ) -> Result<(), VcsError> {
        Self::run(
            ["update-ref", &ref_savepoint_ref(key, branch), at.as_str()],
            repo,
        )
        .map(|_| ())
    }

    fn ref_savepoints(
        &self,
        repo: &Path,
        key: &SavepointKey,
    ) -> Result<Vec<RefSavepoint>, VcsError> {
        Ok(Self::savepoints_under(repo, key)?
            .into_iter()
            .filter_map(|filed| match filed.saved {
                Saved::Ref(branch) => Some(RefSavepoint {
                    branch,
                    at: filed.at,
                }),
                Saved::Checkout(_) => None,
            })
            .collect())
    }

    fn drop_ref_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        branch: &RawRefName,
    ) -> Result<(), VcsError> {
        Self::run(["update-ref", "-d", &ref_savepoint_ref(key, branch)], repo).map(|_| ())
    }

    fn restore_ref_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        saved: &RefSavepoint,
        wrote: Option<&CommitId>,
        foreign_tip_policy: ForeignTipPolicy,
    ) -> Result<RefRestoreOutcome, VcsError> {
        let now = self
            .resolve_local_branch_tip(repo, &saved.branch)?
            .map(|tip| tip.id().clone());
        let Some(now) = now else {
            return Ok(RefRestoreOutcome::Foreign {
                observed: None,
                why: "it is gone".to_owned(),
            });
        };
        if now == saved.at {
            self.drop_ref_savepoint(repo, key, &saved.branch)?;
            return Ok(RefRestoreOutcome::Untouched);
        }
        let held = self.checkouts_of_branch(repo, &saved.branch)?;
        if !held.is_empty() {
            return Ok(RefRestoreOutcome::Foreign {
                observed: Some(now),
                why: format!(
                    "it is checked out at {}",
                    held.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        if Some(&now) == wrote {
            Self::update_branch_ref(repo, &saved.branch, &now, &saved.at)?;
            self.drop_ref_savepoint(repo, key, &saved.branch)?;
            return Ok(RefRestoreOutcome::Restored);
        }
        if foreign_tip_policy == ForeignTipPolicy::Abandon {
            let backup = self.create_backup(
                repo,
                &BackupSource::Branch(saved.branch.clone()),
                &ResolvedRevisionId::from(now.clone()),
                Some(BackupMark::Abandoned),
            )?;
            Self::update_branch_ref(repo, &saved.branch, &now, &saved.at)?;
            self.drop_ref_savepoint(repo, key, &saved.branch)?;
            return Ok(RefRestoreOutcome::AbandonedForeignTip {
                abandoned_tip: now,
                backup,
            });
        }
        Ok(RefRestoreOutcome::Foreign {
            observed: Some(now),
            why: match wrote {
                Some(wrote) => {
                    format!("the op left it at {} and it has moved since", wrote.short())
                }
                None => "the op did not move it there".to_owned(),
            },
        })
    }

    fn savepoint_label(&self, key: &SavepointKey) -> String {
        savepoint_ref(key)
    }

    fn savepoint_namespace(&self) -> String {
        SAVEPOINT_NAMESPACE.to_owned()
    }

    fn resolve_savepoint(&self, repo: &Path, key: &SavepointKey) -> Option<ResolvedRevisionId> {
        self.checkout_savepoint(repo, key)
            .ok()
            .flatten()
            .map(|(_, at)| ResolvedRevisionId::from(at))
    }

    fn drop_savepoint(&self, repo: &Path, key: &SavepointKey) {
        if let Ok(filed) = Self::savepoints_under(repo, key) {
            for filed in filed {
                let _ = Self::run(["update-ref", "-d", &filed.ref_name], repo);
            }
        }
    }

    fn drop_checkout_savepoint(&self, repo: &Path, key: &SavepointKey) -> Result<(), VcsError> {
        for filed in Self::savepoints_under(repo, key)? {
            if let Saved::Checkout(_) = filed.saved {
                Self::run(["update-ref", "-d", &filed.ref_name], repo)?;
            }
        }
        Ok(())
    }

    fn verified_restore_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        attribution: &Attribution<'_>,
        foreign_tip_policy: ForeignTipPolicy,
    ) -> Result<VerifiedRestoreOutcome, VcsError> {
        let mut kept = Vec::new();
        let outcome =
            self.restore_checkout_savepoint(repo, key, attribution, foreign_tip_policy, &mut kept)?;
        Ok(if kept.is_empty() {
            outcome
        } else {
            VerifiedRestoreOutcome::Kept {
                kept,
                then: Box::new(outcome),
            }
        })
    }

    fn cancel_returns_to(&self, repo: &Path) -> Result<Option<ResolvedRevisionId>, VcsError> {
        let started_at = |state_file: &str| -> Result<Option<ResolvedRevisionId>, VcsError> {
            let path = Self::run_trimmed(
                ["rev-parse", "--path-format=absolute", "--git-path", state_file],
                repo,
            )?;
            match std::fs::read_to_string(path) {
                Ok(id) => Self::ref_tip(repo, id.trim()),
                Err(_) => Ok(None),
            }
        };
        Ok(match self.mid_op(repo) {
            None => None,
            Some(ConflictOp::Rebase) => match started_at("rebase-merge/orig-head")? {
                Some(tip) => Some(tip),
                None => started_at("rebase-apply/orig-head")?,
            },
            Some(ConflictOp::CherryPick | ConflictOp::Revert) => match started_at("sequencer/head")? {
                Some(tip) => Some(tip),
                None => Some(self.head_revision(repo)?),
            },
            Some(ConflictOp::Merge) => Some(self.head_revision(repo)?),
        })
    }

    fn cancel_in_flight_op(&self, repo: &Path) {
        let abort_args: &[&str] = match self.mid_op(repo) {
            Some(ConflictOp::Rebase) => &["rebase", "--abort"],
            Some(ConflictOp::Merge) => &["merge", "--abort"],
            Some(ConflictOp::CherryPick) => &["cherry-pick", "--abort"],
            Some(ConflictOp::Revert) => &["revert", "--abort"],
            None => return,
        };
        let _ = Self::run(abort_args, repo);
    }

    fn branch_has_remote_counterpart(
        &self,
        repo: &Path,
        branch: &RawRefName,
    ) -> Result<bool, VcsError> {
        Ok(!Self::remote_counterparts(repo, branch.as_str())?.is_empty())
    }

    fn count_commits_ahead_of_remote(
        &self,
        repo: &Path,
        branch: &RawRefName,
    ) -> Result<usize, VcsError> {
        let mut args = vec![
            "rev-list".to_owned(),
            "--count".to_owned(),
            local_branch(branch.as_str()),
            "--not".to_owned(),
        ];
        args.extend(Self::remote_counterparts(repo, branch.as_str())?);
        let out = Self::run_trimmed(&args, repo)?;
        Ok(out.trim().parse::<usize>().unwrap_or(0))
    }

    fn unpushed_commits(
        &self,
        repo: &Path,
        branch: &RawRefName,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        let tip = local_branch(branch.as_str());
        let superseded = format!("--glob={SUPERSEDED_NAMESPACE}*");
        let args = [
            "log",
            "--pretty=format:%H%x00%s",
            tip.as_str(),
            "--not",
            "--remotes",
            superseded.as_str(),
            "--",
        ];
        Self::run_trimmed(args, repo)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let (id, subject) = line.split_once('\0').unwrap_or((line, ""));
                Ok(CommitSummary {
                    id: commit_id_printed(id, &args, repo)?,
                    subject: subject.to_owned(),
                })
            })
            .collect()
    }

    fn unpushed_beyond(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        base: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        let superseded = format!("--glob={SUPERSEDED_NAMESPACE}*");
        let args = [
            "log",
            "--pretty=format:%H%x00%s",
            tip.id().as_str(),
            "--not",
            base.id().as_str(),
            "--remotes",
            superseded.as_str(),
            "--",
        ];
        Self::run_trimmed(args, repo)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let (id, subject) = line.split_once('\0').unwrap_or((line, ""));
                Ok(CommitSummary {
                    id: commit_id_printed(id, &args, repo)?,
                    subject: subject.to_owned(),
                })
            })
            .collect()
    }

    fn pushed_range(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        pushed: Option<&ResolvedRevisionId>,
    ) -> Result<Vec<RangeCommit>, VcsError> {
        let mut args = vec![
            "rev-list",
            "--parents",
            "--reverse",
            "--topo-order",
            tip.id().as_str(),
            "--not",
        ];
        match pushed {
            Some(pushed) => args.push(pushed.id().as_str()),
            None => args.push("--remotes"),
        }
        Self::run_trimmed(&args[..], repo)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let mut ids = line
                    .split_whitespace()
                    .map(|id| commit_id_printed(id, &args[..], repo));
                let id = ids.next().expect("a listed line names its commit")?;
                Ok(RangeCommit {
                    id,
                    parents: ids.collect::<Result<_, _>>()?,
                })
            })
            .collect()
    }

    fn fetch_objects_from(&self, dst_repo: &Path, src_repo: &Path) {
        // Errors are swallowed by design — for sibling worktrees that
        // share an object store the fetch may fail (FETCH_HEAD unavailable)
        // and yet the objects are already reachable. A real problem
        // surfaces at the subsequent operation (e.g. the ff merge in
        // land step 3) which inspects the same objects. A source path git
        // could not name is the same shape of nothing-to-report.
        let Ok(src_path) = GitArg::from_path(src_repo, "source repo path") else {
            return;
        };
        let _ = git_command()
            .args(Argv::from(["fetch"]).path(&src_path).text("HEAD"))
            .current_dir(dst_repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    fn left_behind_at(&self, repo: &Path) -> Result<Option<CommitId>, VcsError> {
        left_behind_at(repo)
    }

    fn path_standings(
        &self,
        repo: &Path,
        left: Option<&CommitId>,
    ) -> Result<Vec<PathStanding>, VcsError> {
        path_standings(repo, left)
    }

    fn refresh_paths_from_head(
        &self,
        repo: &Path,
        files: &[String],
        index_entries: &[String],
    ) -> Result<(), VcsError> {
        refresh_paths_from_head(repo, files, index_entries)
    }

    fn commit_object_exists(&self, repo: &Path, id: &CommitId) -> Result<bool, VcsError> {
        let deref = format!("{id}^{{commit}}");
        let status = git_command()
            .args(["cat-file", "-e", &deref])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git cat-file -e {deref}"),
                source: e,
            })?;
        Ok(status.success())
    }

    fn refs_unreachable_from(&self, repo: &Path, other: &Path) -> Result<Vec<String>, VcsError> {
        let listing = Self::run(["for-each-ref", "--format=%(objectname) %(refname)"], repo)?;
        let mut tips: Vec<(String, String)> = listing
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(sha, name)| (name.to_owned(), sha.to_owned()))
            .collect();
        if let HeadObservation::Detached { at } = self.observe_head(repo)? {
            tips.push(("HEAD".to_owned(), at.id().as_str().to_owned()));
        }
        let shas: BTreeSet<&str> = tips.iter().map(|(_, sha)| sha.as_str()).collect();
        let held = Self::objects_held(other, &shas)?;
        let mut unreached: BTreeSet<&str> = shas.difference(&held).copied().collect();
        let held: Vec<&str> = held.into_iter().collect();
        // Batched so the argument list stays under Windows' command-line
        // limit; a batch with no output is a batch `other` reaches entirely.
        for batch in held.chunks(200) {
            let mut args = vec!["rev-list", "--max-count=1"];
            args.extend_from_slice(batch);
            args.extend(["--not", "--all"]);
            if Self::run_trimmed(&args, other)?.is_empty() {
                continue;
            }
            for &sha in batch {
                let ahead =
                    Self::run_trimmed(["rev-list", "--max-count=1", sha, "--not", "--all"], other)?;
                if !ahead.is_empty() {
                    unreached.insert(sha);
                }
            }
        }
        let mut names: Vec<String> = tips
            .iter()
            .filter(|(_, sha)| unreached.contains(sha.as_str()))
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        Ok(names)
    }

    fn store_of(&self, workspace: &Path) -> Option<PathBuf> {
        // `--path-format=absolute --git-common-dir` returns the absolute path
        // of the shared object/refs store, regardless of whether `workspace`
        // is a full clone (returns `<workspace>/.git`) or a linked worktree
        // (returns the store path of whichever clone backs it).
        //
        // Equality on the returned path is the load-bearing primitive: two
        // workspaces share an object DAG iff this call returns the same path
        // for both. `rwv doctor`'s clone-topology check uses that equality to
        // enforce I1/I2 from the clone-topology joint.
        if !workspace.exists() {
            return None;
        }
        let raw = Self::run_trimmed(
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            workspace,
        )
        .ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(PathBuf::from(trimmed))
    }

    fn main_checkout(&self, checkout: &Path) -> Option<PathBuf> {
        if !checkout.exists() {
            return None;
        }
        let dirs = Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ],
            checkout,
        )
        .ok()?;
        let [top, own, common] = dirs.lines().collect::<Vec<_>>()[..] else {
            return None;
        };
        if CanonicalPath::of(Path::new(own)) == CanonicalPath::of(Path::new(common)) {
            return Some(PathBuf::from(top));
        }
        let listing =
            Self::run_trimmed_awaiting_peers(["worktree", "list", "--porcelain"], checkout).ok()?;
        let main = listing.split("\n\n").next()?;
        if main.lines().any(|line| line == "bare") {
            return None;
        }
        main.lines()
            .find_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
    }

    fn list_stale_worktree_registrations(&self, repo: &Path) -> Result<Vec<PathBuf>, VcsError> {
        Ok(Self::worktree_records(repo)?
            .into_iter()
            .filter(|record| record.prunable)
            .map(|record| record.path)
            .collect())
    }

    fn live_worktree_branches(&self, repo: &Path) -> Result<Vec<RawRefName>, VcsError> {
        Ok(Self::worktree_records(repo)?
            .into_iter()
            .filter(|record| !record.prunable)
            .filter_map(|record| record.branch)
            .collect())
    }

    fn prune_stale_worktree_registration(
        &self,
        repo: &Path,
        worktree: &Path,
    ) -> Result<RegistrationPrune, VcsError> {
        let Some(record) = Self::worktree_records(repo)?
            .into_iter()
            .find(|record| same_location(&record.path, worktree))
        else {
            return Ok(RegistrationPrune::NotRegistered);
        };
        if record.locked {
            return Ok(RegistrationPrune::Locked);
        }
        if record.path.symlink_metadata().is_ok() || worktree.symlink_metadata().is_ok() {
            return Ok(RegistrationPrune::DirectoryPresent);
        }
        let registered = GitArg::from_path(&record.path, "worktree path")?;
        Self::run_awaiting_peers(Argv::from(["worktree", "remove"]).path(&registered), repo)?;
        Ok(RegistrationPrune::Pruned)
    }

    fn worktree_registration(
        &self,
        store: &Path,
        checkout: &Path,
    ) -> Result<Option<PathBuf>, VcsError> {
        let common = Self::run_trimmed(
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            &Self::work_dir_for_store(store),
        )?;
        let dir = Path::new(&common).join("worktrees");
        let registrations = match std::fs::read_dir(&dir) {
            Ok(registrations) => registrations,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(VcsError::Io {
                    ctx: format!("failed to list {}", dir.display()),
                    source: e,
                })
            }
        };
        let link = store_path_in(checkout);
        Ok(registrations
            .flatten()
            .map(|r| r.path())
            .find(|registration| {
                std::fs::read_to_string(registration.join("gitdir"))
                    .is_ok_and(|named| same_location(Path::new(named.trim()), &link))
            }))
    }

    fn reattach_registered_worktree(
        &self,
        store: &Path,
        checkout: &Path,
        registration: &Path,
    ) -> Result<(), VcsError> {
        let link = store_path_in(checkout);
        std::fs::write(&link, format!("gitdir: {}\n", registration.display())).map_err(|e| {
            VcsError::Io {
                ctx: format!("failed to write {}", link.display()),
                source: e,
            }
        })?;
        let checkout_arg = GitArg::from_path(checkout, "worktree path")?;
        Self::run(
            Argv::from(["worktree", "repair"]).path(&checkout_arg),
            &Self::work_dir_for_store(store),
        )?;
        Ok(())
    }

    fn register_worktree_in_place(
        &self,
        store: &Path,
        checkout: &Path,
        branch: &RawRefName,
        create_at: Option<&ResolvedRevisionId>,
    ) -> Result<(), VcsError> {
        let store = Self::work_dir_for_store(store);
        let io = |ctx: String| move |source| VcsError::Io { ctx, source };
        let (Some(parent), Some(leaf)) = (checkout.parent(), checkout.file_name()) else {
            return Err(io(format!("{} names no directory", checkout.display()))(
                std::io::Error::from(std::io::ErrorKind::InvalidInput),
            ));
        };
        // `git worktree add` will not register a directory that already holds
        // files, so the registration is made on an empty sibling under the
        // same leaf name and its link moved into `checkout`.
        let staging = parent.join(format!(".{}.rwv-relink", leaf.to_string_lossy()));
        let staged = staging.join(leaf);
        std::fs::create_dir(&staging).map_err(io(format!(
            "failed to create {} to register {}",
            staging.display(),
            checkout.display()
        )))?;
        let staged_arg = GitArg::from_path(&staged, "worktree path")?;
        let add = Argv::from(["worktree", "add", "--no-checkout"]);
        let add = match create_at {
            Some(at) => add
                .text("-b")
                .text(branch.as_str())
                .path(&staged_arg)
                .text(at.id().as_str()),
            None => add.path(&staged_arg).text(branch.as_str()),
        };
        if let Err(e) = Self::add_awaiting_peers(add, &store, &staged) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
        let link = store_path_in(checkout);
        if std::fs::symlink_metadata(&link).is_ok() {
            std::fs::remove_file(&link)
                .map_err(io(format!("failed to remove {}", link.display())))?;
        }
        std::fs::rename(store_path_in(&staged), &link).map_err(io(format!(
            "failed to move the new link into {}",
            link.display()
        )))?;
        std::fs::remove_dir(&staged)
            .and_then(|()| std::fs::remove_dir(&staging))
            .map_err(io(format!("failed to remove {}", staging.display())))?;
        let checkout_arg = GitArg::from_path(checkout, "worktree path")?;
        Self::run(
            Argv::from(["worktree", "repair"]).path(&checkout_arg),
            &store,
        )?;
        Self::run(["reset", "--quiet"], checkout)?;
        Ok(())
    }

    fn write_backup(
        &self,
        repo: &Path,
        source: &BackupSource,
        tip: &ResolvedRevisionId,
        mark: Option<BackupMark>,
    ) -> Result<Backup, VcsError> {
        Self::write_backup_ref(repo, source, tip, mark)
    }

    fn list_backups(
        &self,
        repo: &Path,
        remote_tracking: RemoteTracking,
    ) -> Result<Vec<ListedBackup>, VcsError> {
        Self::backups(repo, remote_tracking)
    }

    fn drop_backup(&self, repo: &Path, name: &BackupName) -> Result<(), VcsError> {
        Self::drop_backup_ref(repo, name)
    }

    fn reached_by_no_branch(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        remote_tracking: RemoteTracking,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        Self::commits_reached_by_no_branch(&Self::work_dir_for_store(repo), tip, remote_tracking)
    }

    fn snapshot_edits(&self, repo: &Path) -> Result<Option<ResolvedRevisionId>, VcsError> {
        Self::snapshot_working_tree(repo)
    }

    fn branch_tips(&self, repo: &Path) -> Result<Vec<BranchTip>, VcsError> {
        Self::branch_tip_listing(repo)
    }

    fn records_branch(&self) -> &'static str {
        records::RECORDS_BRANCH
    }

    fn records_tip(
        &self,
        repo: &Path,
        site: RecordsSite<'_>,
    ) -> Result<Option<CommitId>, VcsError> {
        Self::records_tip_at(&Self::work_dir_for_store(repo), site)
    }

    fn advance_records(
        &self,
        repo: &Path,
        expected: Option<&CommitId>,
        to: &CommitId,
    ) -> Result<RecordsAdvance, VcsError> {
        Self::advance_records_ref(&Self::work_dir_for_store(repo), expected, to)
    }

    fn publish_records(
        &self,
        repo: &Path,
        remote: &RemoteHandle,
        commit: &CommitId,
    ) -> Result<RecordsPublish, VcsError> {
        Self::publish_records_commit(&Self::work_dir_for_store(repo), remote, commit)
    }

    fn files_at(&self, repo: &Path, commit: &CommitId) -> Result<Vec<CommittedFile>, VcsError> {
        Self::files_of_commit(&Self::work_dir_for_store(repo), commit)
    }

    fn write_commit(
        &self,
        repo: &Path,
        parents: &[CommitId],
        changes: &[FileChange],
        message: &str,
    ) -> Result<CommitId, VcsError> {
        Self::write_tree_commit(&Self::work_dir_for_store(repo), parents, changes, message)
    }

    fn merge_bases(
        &self,
        repo: &Path,
        a: &CommitId,
        b: &CommitId,
    ) -> Result<Vec<CommitId>, VcsError> {
        Self::merge_base_list(&Self::work_dir_for_store(repo), a, b)
    }

    fn merge_base_against(
        &self,
        repo: &Path,
        commit: &CommitId,
        merged: &[CommitId],
    ) -> Result<Option<CommitId>, VcsError> {
        Self::merge_base_of_merge(&Self::work_dir_for_store(repo), commit, merged)
    }

    fn last_message_touching(
        &self,
        repo: &Path,
        tip: &CommitId,
        path: &str,
    ) -> Result<Option<String>, VcsError> {
        Self::last_message_changing(&Self::work_dir_for_store(repo), tip, path)
    }

    fn branch_push_targets(
        &self,
        repo: &Path,
        url_remote: Option<&RemoteHandle>,
    ) -> Result<Vec<RemoteHandle>, VcsError> {
        Self::push_target_remotes(&Self::work_dir_for_store(repo), url_remote)
    }

    fn named_branches(&self, repo: &Path) -> Result<Vec<NamedBranch>, VcsError> {
        Self::every_branch(&Self::work_dir_for_store(repo))
    }

    fn list_savepoints(&self, repo: &Path) -> Result<SavepointListing, VcsError> {
        let prefix = format!("{SAVEPOINT_NAMESPACE}/");
        let output = Self::run_trimmed(["for-each-ref", "--format=%(refname)", &prefix], repo)?;
        let mut listing = SavepointListing::default();
        for refname in output.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let filed =
                refname
                    .strip_prefix(&prefix)
                    .and_then(|below| match below.split_once('/') {
                        Some((key, rest)) => {
                            Saved::parse(rest)?;
                            key.parse::<SavepointKey>().ok()
                        }
                        None => below.parse::<SavepointKey>().ok(),
                    });
            match filed {
                Some(key) if listing.keys.contains(&key) => {}
                Some(key) => listing.keys.push(key),
                None => listing.unrecognized.push(refname.to_owned()),
            }
        }
        Ok(listing)
    }

    fn file_versions(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        path: &Path,
    ) -> Result<Vec<String>, VcsError> {
        let file = GitArg::from_path(path, "file path")?;
        let listing = Self::run(
            [
                "log",
                "--raw",
                "--no-abbrev",
                "--no-renames",
                "--format=",
                tip.id().as_str(),
                "--",
                file.as_str(),
            ],
            repo,
        )?;
        let mut blobs: Vec<&str> = Vec::new();
        for line in listing.lines() {
            let Some((meta, _)) = line.split_once('\t') else {
                continue;
            };
            let Some(new) = meta.split_whitespace().nth(3) else {
                continue;
            };
            if new.bytes().any(|b| b != b'0') && !blobs.contains(&new) {
                blobs.push(new);
            }
        }
        blobs
            .into_iter()
            .map(|blob| Self::run(["cat-file", "blob", blob], repo))
            .collect()
    }

    fn read_file_at_revision(
        &self,
        repo: &Path,
        revision: &ResolvedRevisionId,
        file_path: &Path,
    ) -> Result<String, VcsError> {
        let file = GitArg::from_path(file_path, "file path")?;
        let rev_path = format!("{}:{}", revision.id().as_str(), file.as_str());
        let failure = match Self::run(["show", &rev_path], repo) {
            Ok(content) => return Ok(content),
            Err(failure @ VcsError::CommandFailed { .. }) => failure,
            Err(e) => return Err(e),
        };
        // Naming the entry reads only trees, so a partial clone answers
        // without fetching the blob whose absence may be the failure.
        let named = Self::run_or_absent(
            ["rev-parse", "--verify", "--quiet", &rev_path],
            repo,
            Absent::Revision,
        );
        match named {
            Ok(None) => Err(VcsError::RevisionNotFound {
                repo: repo.to_path_buf(),
                rev: rev_path,
            }),
            _ => Err(failure),
        }
    }

    fn rebase_stopped_commit_detail(&self, repo: &Path) -> String {
        GitVcs::rebase_stopped_commit_detail_impl(repo)
    }

    fn commits_in_range(
        &self,
        repo: &Path,
        from: &CommitId,
        to: &CommitId,
        cap: usize,
    ) -> (Vec<CommitSummary>, usize) {
        GitVcs::capped_log_range(repo, from, to, cap)
    }

    fn ahead_behind(&self, repo: &Path, savepoint: &CommitId, tip: &CommitId) -> (usize, usize) {
        GitVcs::ahead_behind_impl(repo, savepoint, tip)
    }

    fn commits_beyond(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        base: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        Self::log_range(repo, base.id(), tip.id(), None)
    }

    fn commits_excluding(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        excluding: &[CommitId],
        superseded: bool,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        let glob = format!("--glob={SUPERSEDED_NAMESPACE}*");
        let mut args = vec![
            "log",
            "--pretty=format:%H%x00%s",
            tip.id().as_str(),
            "--not",
        ];
        args.extend(excluding.iter().map(CommitId::as_str));
        if superseded {
            args.push(glob.as_str());
        }
        args.push("--");
        Self::run_trimmed(&args[..], repo)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let (id, subject) = line.split_once('\0').unwrap_or((line, ""));
                Ok(CommitSummary {
                    id: commit_id_printed(id, &args[..], repo)?,
                    subject: subject.to_owned(),
                })
            })
            .collect()
    }

    fn first_parent_chain(&self, repo: &Path, tip: &CommitId) -> Result<Vec<CommitId>, VcsError> {
        let args = ["rev-list", "--first-parent", tip.as_str()];
        Self::run_trimmed(args, repo)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|id| commit_id_printed(id, &args, repo))
            .collect()
    }

    fn commits_changing(
        &self,
        repo: &Path,
        tip: &CommitId,
        path: &str,
    ) -> Result<Vec<CommitId>, VcsError> {
        let args = ["rev-list", "--full-history", tip.as_str(), "--", path];
        Self::run_trimmed(args, repo)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|id| commit_id_printed(id, &args, repo))
            .collect()
    }

    fn patch_equivalents(
        &self,
        repo: &Path,
        unlanded: &CommitId,
        onto: &CommitId,
    ) -> Option<PatchEquivalents> {
        let base = Self::merge_base(repo, unlanded, onto).ok()?;
        let mut found = PatchEquivalents::default();
        let cherry = Self::run_trimmed(["cherry", onto.as_str(), unlanded.as_str()], repo).ok()?;
        let upstream_ids = |range: &str| -> Option<Vec<(String, CommitId)>> {
            let log = Self::run(["log", "-p", "--no-color", "--first-parent", range], repo).ok()?;
            let ids = Self::run_with_stdin(["patch-id", "--stable"], repo, log.as_bytes()).ok()?;
            ids.lines()
                .filter_map(|line| line.split_once(' '))
                .map(|(patch, commit)| Some((patch.to_owned(), CommitId::parse(commit).ok()?)))
                .collect()
        };
        let on_onto = upstream_ids(&format!("{base}..{onto}"))?;
        let mine = upstream_ids(&format!("{base}..{unlanded}"))?;
        for line in cherry.lines() {
            let Some((mark, id)) = line.split_once(' ') else {
                continue;
            };
            let id = CommitId::parse(id).ok()?;
            match mark {
                "-" => {
                    let patch = mine.iter().find(|(_, c)| *c == id).map(|(p, _)| p);
                    let twin = patch
                        .and_then(|patch| on_onto.iter().find(|(p, _)| p == patch))
                        .map(|(_, c)| c.clone());
                    if let Some(twin) = twin {
                        found.matched.push((id, twin));
                    }
                }
                _ => found.unmatched.push(id),
            }
        }
        let combined = Self::run(
            ["diff", "--no-color", base.as_str(), unlanded.as_str()],
            repo,
        )
        .ok()?;
        if !combined.trim().is_empty() {
            let id =
                Self::run_with_stdin(["patch-id", "--stable"], repo, combined.as_bytes()).ok()?;
            if let Some(patch) = id.split_whitespace().next() {
                found.combined = on_onto
                    .iter()
                    .find(|(p, _)| p == patch)
                    .map(|(_, c)| c.clone());
            }
        }
        Some(found)
    }

    fn diff_beyond(&self, repo: &Path, base: &ResolvedRevisionId) -> Result<DiffBeyond, VcsError> {
        let tip = Self::rev_parse(repo, "HEAD")?;
        let anchor = Self::merge_base(repo, base.id(), &tip)?;
        let text = Self::diff_range(repo, &anchor, &tip)?;
        Ok(DiffBeyond {
            base: Some(anchor),
            text,
        })
    }

    fn log(&self, repo: &Path, range: Option<&str>) -> Result<RangeListing, VcsError> {
        let Some(range) = range else {
            return Ok(RangeListing {
                endpoints: Vec::new(),
                commits: Self::log_spelled(repo, "HEAD", None)?,
            });
        };
        let endpoints = range_endpoints(range)
            .into_iter()
            .map(|spelled| {
                Ok(ResolvedEndpoint {
                    id: Self::endpoint_commit(repo, spelled)?,
                    spelled: spelled.to_owned(),
                })
            })
            .collect::<Result<Vec<_>, VcsError>>()?;
        Ok(RangeListing {
            endpoints,
            commits: Self::log_spelled(repo, range, None)?,
        })
    }

    // =======================================================================
    // The branch model
    // =======================================================================

    fn observe_head(&self, repo: &Path) -> Result<HeadObservation, VcsError> {
        // "Not a repo" is an error, not a state. The shipped `current_ref`
        // folded it into the same `Ok(None)` as "detached", which is how
        // `rwv push` came to report a detached HEAD for a directory with no
        // git in it at all.
        if !self.is_repo(repo) {
            return Err(VcsError::NotARepo(repo.to_path_buf()));
        }
        // The full ref, then strip the namespace here. `--short` would ask
        // git for the shortest *unambiguous* name instead of the branch
        // name: with a tag of the same name present it answers `heads/main`
        // for the branch `main`, and that value is not a branch name — it
        // does not round-trip through `refs/heads/<name>`, so a witness
        // carrying it would report an existing branch as missing.
        match Self::run_trimmed(["symbolic-ref", "HEAD"], repo) {
            Ok(full) => {
                let name = Self::local_branch_name_from_full_ref(full.trim(), repo)?;
                // HEAD is symbolic. Whether the branch has commits is a
                // second question: `symbolic-ref` succeeds on an unborn
                // branch, and `rev-parse HEAD` is what tells the two apart.
                // (This is the check `head_revision` had to grow inline; it
                // belongs here, where the question is actually asked.)
                match Self::run(["rev-parse", "--verify", "HEAD^{commit}"], repo) {
                    Ok(_) => Ok(HeadObservation::Attached { name }),
                    Err(_) => Ok(HeadObservation::Unborn { name }),
                }
            }
            Err(symbolic_err) => {
                // HEAD is not symbolic — or the ref database is unreadable.
                // Resolving HEAD tells us which: a detached HEAD resolves,
                // a broken refdb does not, and the latter is an error.
                match Self::run_trimmed(["rev-parse", "--verify", "HEAD^{commit}"], repo) {
                    Ok(sha) => match ResolvedRevisionId::from_rev_parse_output(&sha) {
                        Some(at) => Ok(HeadObservation::Detached { at }),
                        None => Err(VcsError::CommandFailed {
                            args: vec!["rev-parse".to_owned(), "HEAD^{commit}".to_owned()],
                            repo: repo.to_path_buf(),
                            stderr: format!("HEAD resolved to a non-canonical value: {sha:?}"),
                        }),
                    },
                    Err(_) => Err(symbolic_err),
                }
            }
        }
    }

    fn resolve_local_branch_tip(
        &self,
        repo: &Path,
        name: &RawRefName,
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        let repo = Self::work_dir_for_store(repo);
        let args = local_branch_tip_args(name.as_str());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        match Self::run_trimmed(&argv, &repo) {
            Ok(sha) => Ok(ResolvedRevisionId::from_rev_parse_output(&sha)),
            // `--quiet` makes "no such ref" an exit code with no stderr,
            // which is the absence this method reports rather than an error.
            Err(VcsError::CommandFailed { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn mid_operation(&self, repo: &Path) -> Option<MidOperation> {
        Self::mid_op_state(repo)
    }

    fn materialize_worktree_on_ref(
        &self,
        store: &Path,
        dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<RefOrigin, VcsError> {
        self.add_worktree(store, dest, name, start_point, WorktreeFiles::Written)
    }

    fn clone_attached_at(
        &self,
        url: &CloneUrl,
        dest: &Path,
        name: &LocalRefName,
        declared: &RemoteRef,
        pin: Option<&RawRevisionId>,
    ) -> Result<MemberBirth, VcsError> {
        let dest_arg = GitArg::from_path(dest, "destination path")?;
        // `--no-checkout`: the working tree is never materialized at the
        // remote tip. Cloning still writes the remote's default branch as a
        // local ref — git offers no way to decline that — but no working
        // tree hangs off it, nothing has observed it, and rwv issues no
        // MOVE against it. That last part is what matters: bootstrapping
        // from a lock behind origin must not require a rewind's consent.
        Self::run(
            Argv::from(["clone", "--no-checkout"])
                .text(url.as_str())
                .path(&dest_arg),
            Path::new("."),
        )?;
        let remote = Self::only_remote(dest)?;
        // Resolved in the new clone's own object store, so a pin origin has
        // but this clone does not is a resolution failure rather than a
        // silent network fetch — the same locality the present-clone arm has.
        let (at, outcome) = match pin {
            Some(pin) => match self.resolve_revision(dest, pin.as_str()) {
                Ok(at) => (at, PinOutcome::Reached),
                Err(VcsError::RevisionNotFound { .. }) => (
                    self.resolve_revision(dest, &remote_tracking(&remote, declared.branch()))?,
                    PinOutcome::Unreachable,
                ),
                Err(e) => return Err(e),
            },
            None => (
                self.resolve_revision(dest, &remote_tracking(&remote, declared.branch()))?,
                PinOutcome::None,
            ),
        };
        // `-B` names the ref the clone just minted; with an explicit start
        // point there is no `checkout.guess` and no tag lookup, and the `--`
        // terminator keeps a path-shaped `name` from being read as a
        // pathspec — the three misreads `attach_head_to` documents.
        Self::run(
            ["checkout", "-B", name.as_str(), at.id().as_str(), "--"],
            dest,
        )?;
        Ok(MemberBirth { at, pin: outcome })
    }

    fn set_detached_head(&self, repo: &Path, to: &ResolvedRevisionId) -> Result<(), VcsError> {
        Self::run(["checkout", "--detach", to.id().as_str()], repo)?;
        Ok(())
    }

    fn create_branch_and_attach(
        &self,
        repo: &Path,
        name: &RawRefName,
        at: &ResolvedRevisionId,
        departure: &crate::alignment::Departure,
    ) -> Result<(), VcsError> {
        let _ = departure;
        if self.resolve_local_branch_tip(repo, name)?.is_some() {
            return Err(VcsError::CommandFailed {
                args: local_branch_tip_args(name.as_str()),
                repo: repo.to_path_buf(),
                stderr: format!("a local branch named '{name}' already exists"),
            });
        }
        Self::run(
            [
                "checkout",
                "--no-guess",
                "--no-track",
                "-b",
                name.as_str(),
                at.id().as_str(),
                "--",
            ],
            repo,
        )?;
        Ok(())
    }

    fn attach_head_to(&self, repo: &Path, name: &LocalRefName) -> Result<(), VcsError> {
        // Classify before acting, the way `materialize_worktree_on_ref`
        // does. Omitting `-b` does NOT make git refuse an absent branch —
        // measured, all three on git 2.43:
        //
        //   * `checkout.guess` (on by default) invents a local branch from
        //     a remote-tracking one of the same name and reports "Switched
        //     to a new branch", a branch no caller asked for. It is also the
        //     ordinary case
        //     here: a `LocalRefName` is a projection of a *remote* branch
        //     name, and rwv clones have exactly one remote.
        //   * a name matching a tag detaches HEAD and exits 0 — the very
        //     operation this one is separated from, done while holding only
        //     a ReattachConsent.
        //   * a name matching a path (`docs`, `src`, `test`) is taken as a
        //     pathspec: HEAD does not move and the operator's uncommitted
        //     edits to that path are reverted from the index.
        //
        // `--no-guess` closes the first, the `--` terminator closes the
        // third, and *neither* closes the second. So the existence check is
        // what makes the refusal real; the flags are defense in depth for
        // the window between the check and the switch.
        let branch = RawRefName::new(name.as_str());
        if self.resolve_local_branch_tip(repo, &branch)?.is_none() {
            // Reported as the query that refused, not as a switch that
            // failed: no switch is attempted. These are the arguments
            // `resolve_local_branch_tip` ran, verbatim, and the directory it
            // ran them in, so the operator can reproduce the answer the
            // refusal rests on. `--quiet` is part of the command, not noise:
            // without it the same rev-parse exits 128 "Needed a single
            // revision" instead of the silent exit 1 that the absence branch
            // reads as "no such ref", so a reported form omitting it would
            // not reproduce the answer it claims to explain.
            return Err(VcsError::CommandFailed {
                args: local_branch_tip_args(name.as_str()),
                repo: Self::work_dir_for_store(repo),
                stderr: format!(
                    "no local branch named '{name}': attaching to a branch that \
                     does not exist would create it, and rwv attaches only to a branch \
                     that exists"
                ),
            });
        }
        Self::run(["checkout", "--no-guess", name.as_str(), "--"], repo)?;
        Ok(())
    }

    fn delete_branch(&self, proof: DestroyProof) -> Result<(), VcsError> {
        let store = Self::work_dir_for_store(proof.store());
        let branch = proof.branch();
        let tip = proof.tip().id().as_str();
        match proof.site() {
            Site::Local => {
                if let Some(checkout) = self.checkouts_of_branch(&store, branch)?.first() {
                    return Err(VcsError::CommandFailed {
                        args: vec![
                            "update-ref".to_owned(),
                            "-d".to_owned(),
                            local_branch(branch.as_str()),
                        ],
                        repo: store,
                        stderr: format!(
                            "branch '{branch}' is checked out at {}",
                            checkout.display()
                        ),
                    });
                }
                Self::run_awaiting_peers(
                    ["update-ref", "-d", &local_branch(branch.as_str()), tip],
                    &store,
                )?;
                if Self::has_branch_section(&store, branch.as_str())? {
                    Self::run(
                        ["config", "--remove-section", &format!("branch.{branch}")],
                        &store,
                    )?;
                }
            }
            Site::Remote(remote) => {
                let ref_name = local_branch(branch.as_str());
                Self::run(
                    [
                        "push",
                        &format!("--force-with-lease={ref_name}:{tip}"),
                        remote.name(),
                        &format!(":{ref_name}"),
                    ],
                    &store,
                )?;
            }
        }
        Ok(())
    }

    fn private_dir(&self, checkout: &Path) -> Result<PathBuf, VcsError> {
        Ok(PathBuf::from(Self::run_trimmed(
            ["rev-parse", "--absolute-git-dir"],
            checkout,
        )?))
    }

    fn checkouts_of_branch(
        &self,
        repo: &Path,
        branch: &RawRefName,
    ) -> Result<Vec<PathBuf>, VcsError> {
        Ok(Self::worktree_records(repo)?
            .into_iter()
            .filter(|record| !record.prunable)
            .filter(|record| record.branch.as_ref() == Some(branch))
            .map(|record| record.path)
            .collect())
    }

    fn upstream_of(&self, repo: &Path, branch: &RawRefName) -> Result<Option<Upstream>, VcsError> {
        let remote = Self::config_get(repo, &format!("branch.{branch}.remote"))?;
        let merge = Self::config_get(repo, &format!("branch.{branch}.merge"))?;
        Ok(match (remote, merge) {
            (Some(remote), Some(merge)) if remote != "." => merge
                .strip_prefix(LOCAL_BRANCH_NAMESPACE)
                .map(|upstream| Upstream {
                    remote: RemoteHandle::new(remote),
                    branch: RawRefName::new(upstream),
                }),
            _ => None,
        })
    }

    fn remote_copies(&self, repo: &Path, branch: &RawRefName) -> Result<Vec<RemoteTip>, VcsError> {
        let mut found = Vec::new();
        for name in Self::remote_names(repo)? {
            let remote = RemoteHandle::new(name);
            let qualified = remote_tracking(&remote, branch.as_str());
            match self.resolve_revision(repo, &qualified) {
                Ok(tip) => found.push(RemoteTip { remote, tip }),
                Err(VcsError::RevisionNotFound { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(found)
    }

    fn set_upstream(
        &self,
        repo: &Path,
        branch: &RawRefName,
        upstream: &Upstream,
    ) -> Result<(), VcsError> {
        Self::run(
            [
                "config",
                &format!("branch.{branch}.remote"),
                upstream.remote.name(),
            ],
            repo,
        )?;
        Self::run(
            [
                "config",
                &format!("branch.{branch}.merge"),
                &local_branch(upstream.branch.as_str()),
            ],
            repo,
        )?;
        Ok(())
    }

    fn rename_branch(
        &self,
        repo: &Path,
        from: &RawRefName,
        to: &RawRefName,
    ) -> Result<(), VcsError> {
        if self.resolve_local_branch_tip(repo, to)?.is_some() {
            return Err(VcsError::CommandFailed {
                args: local_branch_tip_args(to.as_str()),
                repo: repo.to_path_buf(),
                stderr: format!("a local branch named '{to}' already exists"),
            });
        }
        Self::run(["branch", "--move", from.as_str(), to.as_str()], repo)?;
        if Self::config_get(repo, &format!("branch.{to}.merge"))?.is_some() {
            Self::run(["branch", "--unset-upstream", to.as_str()], repo)?;
        }
        Ok(())
    }

    fn create_branch(
        &self,
        repo: &Path,
        name: &RawRefName,
        at: &ResolvedRevisionId,
        upstream: Option<&Upstream>,
    ) -> Result<(), VcsError> {
        if self.resolve_local_branch_tip(repo, name)?.is_some() {
            return Err(VcsError::CommandFailed {
                args: local_branch_tip_args(name.as_str()),
                repo: repo.to_path_buf(),
                stderr: format!("a local branch named '{name}' already exists"),
            });
        }
        Self::run(
            ["branch", "--no-track", name.as_str(), at.id().as_str()],
            repo,
        )?;
        if let Some(upstream) = upstream {
            self.set_upstream(repo, name, upstream)?;
        }
        Ok(())
    }

    fn stray_commits(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        leaving: Option<&RawRefName>,
        onto: Option<&ResolvedRevisionId>,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        let backups = format!("--glob={BACKUP_NAMESPACE}/*");
        let mut args: Vec<String> = vec![
            "log".to_owned(),
            "--pretty=format:%H%x00%s".to_owned(),
            tip.id().as_str().to_owned(),
            "--not".to_owned(),
        ];
        if let Some(leaving) = leaving {
            args.push(format!("--exclude={}", leaving.as_str()));
        }
        args.extend(["--branches".to_owned(), "--remotes".to_owned(), backups]);
        if let Some(onto) = onto {
            args.push(onto.id().as_str().to_owned());
        }
        args.push("--".to_owned());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        Self::run_trimmed(argv.as_slice(), repo)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let (id, subject) = line.split_once('\0').unwrap_or((line, ""));
                Ok(CommitSummary {
                    id: commit_id_printed(id, &argv, repo)?,
                    subject: subject.to_owned(),
                })
            })
            .collect()
    }

    fn materialize_worktree_detached(
        &self,
        store: &Path,
        dest: &Path,
        at: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        self.add_detached_worktree(store, dest, at, WorktreeFiles::Written)
    }

    fn push_ref(
        &self,
        repo: &Path,
        target: &RemoteHandle,
        r: &PublishRef,
        replace: &Replace,
    ) -> Result<(), VcsError> {
        let branch = local_branch(r.name().as_str());
        let lease = match replace {
            Replace::FastForwardOnly => None,
            Replace::Leased(lease) => Some(format!(
                "--force-with-lease={branch}:{}",
                lease.expected().id()
            )),
        };
        let mut args: Vec<&str> = vec!["push"];
        args.extend(lease.as_deref());
        args.push(target.name());
        let refspec = format!("{}:{branch}", r.commit().id().as_str());
        args.push(&refspec);
        Self::run(&args, repo)?;
        Ok(())
    }

    fn remote_default_branch(
        &self,
        repo: &Path,
        remote: &RemoteHandle,
    ) -> Result<Option<RemoteDefaultBranch>, VcsError> {
        // A non-repo is an error; an unset symref is an absence. Keeping
        // those apart is the same move `observe_head` makes, applied to the
        // other side of the L1 publish gate.
        if !self.is_repo(repo) {
            return Err(VcsError::NotARepo(repo.to_path_buf()));
        }
        match Self::run_trimmed(["symbolic-ref", &remote_tracking(remote, "HEAD")], repo) {
            // No fallback: an unset or malformed symref is an absence, not
            // a guess. A caller that invents a name here trades an honest
            // refusal for a comparison against a fabrication.
            Ok(target) => Ok(RemoteDefaultBranch::from_symref_target(
                &target,
                &remote_tracking_namespace(remote),
            )),
            Err(_) => Ok(None),
        }
    }

    fn remote_default_branch_repair_hint(&self, remote: &RemoteHandle) -> String {
        let remote = remote.name();
        format!(
            "{remote}/HEAD is unset; run `git fetch {remote} && git remote set-head \
             {remote} -a` there to record its canonical branch"
        )
    }

    fn clone_default_branch_repair_hint(
        &self,
        clone: &Path,
        remote: &RemoteHandle,
        copies: RemoteBranchCopies,
    ) -> Result<String, VcsError> {
        let namespace = remote_tracking_namespace(remote);
        let remote = remote.name();
        let listing =
            Self::run_trimmed(["for-each-ref", "--format=%(refname)", &namespace], clone)?;
        let branches: Vec<String> = listing
            .lines()
            .filter_map(|r| r.trim().strip_prefix(namespace.as_str()))
            .filter(|b| *b != "HEAD")
            .map(|b| format!("`{b}`"))
            .collect();
        if !branches.is_empty() {
            return Ok(format!(
                "{remote}/HEAD is unset, and {remote} has {}; check out the one to track \
                 there and record it (`git checkout <branch> && git remote set-head {remote} \
                 <branch>`)",
                branches.join(", ")
            ));
        }
        let branch = match self.head_attachment(clone)? {
            HeadAttachment::Unborn(u) => u.to_string(),
            HeadAttachment::Attached(a) => a.to_string(),
            HeadAttachment::Detached(_) => "<branch>".to_owned(),
        };
        let first_push = format!(
            "commit there, push the branch (`git push -u {remote} {branch}`) and record it \
             (`git remote set-head {remote} {branch}`)"
        );
        Ok(match copies {
            RemoteBranchCopies::JustCloned => {
                format!("{remote} has no branch there, so {remote}/HEAD is unset; {first_push}")
            }
            RemoteBranchCopies::AsLastFetched => format!(
                "{remote}/HEAD is unset and no branch of {remote} has been fetched there; run \
                 `git fetch {remote}` there, then, if {remote} has a branch, record its \
                 canonical one (`git remote set-head {remote} -a`), and if it has none, \
                 {first_push}"
            ),
        })
    }

    fn list_branch_names_with_prefix(
        &self,
        repo: &Path,
        prefix: &str,
    ) -> Result<Vec<RawRefName>, VcsError> {
        // Filtered here rather than by a `refs/heads/<prefix>*` pattern:
        // git matches ref patterns with `*` stopping at `/`, so the glob
        // silently drops `<prefix>deep/inner` while the contract above says
        // "starting with `prefix`". A listing that omits a leftover ref
        // reports it as absent.
        Ok(self
            .list_local_branch_names(repo)?
            .into_iter()
            .filter(|n| n.as_str().starts_with(prefix))
            .collect())
    }

    fn list_local_branch_names(&self, repo: &Path) -> Result<Vec<RawRefName>, VcsError> {
        let output = Self::run_trimmed(
            [
                "for-each-ref",
                "--format=%(refname)",
                LOCAL_BRANCH_NAMESPACE,
            ],
            repo,
        )?;
        Ok(output
            .lines()
            .filter_map(|full| full.trim().strip_prefix(LOCAL_BRANCH_NAMESPACE))
            .filter(|name| !name.is_empty())
            .map(RawRefName::new)
            .collect())
    }

    fn population(&self) -> Option<&dyn WorkspacePopulation> {
        Some(self)
    }

    fn tree_merges(&self) -> Option<&dyn TreeMerges> {
        git_merges_trees().then_some(self as &dyn TreeMerges)
    }

    fn start_merge(&self, checkout: &Path, other: &CommitId) -> Result<Vec<String>, VcsError> {
        Self::begin_checkout_merge(checkout, other)
    }

    fn merge_in_flight(&self, checkout: &Path) -> Result<Option<CommitId>, VcsError> {
        Self::merge_head(checkout)
    }

    fn copy_commit(
        &self,
        checkout: &Path,
        commit: &CommitId,
        mainline: Option<usize>,
    ) -> Result<Copied, VcsError> {
        Self::copy_commit_onto(checkout, commit, mainline)
    }

    fn continue_copy(&self, checkout: &Path) -> Result<Copied, VcsError> {
        Self::continue_copy_in(checkout)
    }

    fn copy_uncommitted(
        &self,
        checkout: &Path,
        commit: &CommitId,
        mainline: Option<usize>,
    ) -> Result<Vec<String>, VcsError> {
        Self::copy_without_commit(checkout, commit, mainline)
    }

    fn copy_in_flight(&self, checkout: &Path) -> Result<Option<CommitId>, VcsError> {
        Self::cherry_pick_head(checkout)
    }

    fn copied_from(&self, repo: &Path, commit: &CommitId) -> Result<Option<CommitId>, VcsError> {
        Self::cherry_picked_from(repo, commit)
    }

    fn parents(&self, repo: &Path, commit: &CommitId) -> Result<Vec<CommitId>, VcsError> {
        Self::parents_of(repo, commit)
    }

    fn message_without(
        &self,
        repo: &Path,
        commit: &CommitId,
        keys: &[&str],
    ) -> Result<String, VcsError> {
        Self::commit_message_without(repo, commit, keys)
    }

    fn unmerged_paths(&self, checkout: &Path) -> Result<Vec<String>, VcsError> {
        Self::unmerged(checkout)
    }

    fn staged_file(&self, checkout: &Path, path: &str) -> Result<Option<String>, VcsError> {
        Self::staged(checkout, path)
    }

    fn commit_merge(
        &self,
        checkout: &Path,
        first: &CommitId,
        second: &CommitId,
        message: &str,
        annotations: &[Annotation],
    ) -> Result<CommitId, VcsError> {
        Self::commit_staged_merge(
            checkout,
            first,
            second,
            &with_trailers(message, annotations),
        )
    }

    fn merge_as_refs(
        &self,
        repo: &Path,
        first: &CommitId,
        second: &CommitId,
        message: &str,
    ) -> Result<RefMerge, VcsError> {
        self.merge_commit_as_refs(repo, first, second, message)
    }

    fn advance_branch_ref(
        &self,
        repo: &Path,
        branch: &RawRefName,
        expected: &CommitId,
        to: &CommitId,
    ) -> Result<(), VcsError> {
        if !Self::descends(repo, expected.as_str(), to.as_str())? {
            return Err(VcsError::CommandFailed {
                args: vec!["update-ref".to_owned(), local_branch(branch.as_str())],
                repo: repo.to_path_buf(),
                stderr: "not a fast-forward".to_owned(),
            });
        }
        Self::update_branch_ref(repo, branch, expected, to)
    }

    fn restore_branch_ref(
        &self,
        repo: &Path,
        branch: &RawRefName,
        moved_to: &CommitId,
        original: &CommitId,
    ) -> Result<(), VcsError> {
        Self::update_branch_ref(repo, branch, moved_to, original)
    }

    fn reset_branch_ref(
        &self,
        repo: &Path,
        branch: &RawRefName,
        expected: &CommitId,
        to: &CommitId,
        kept: &BackupWitness,
    ) -> Result<(), VcsError> {
        Self::witness_backs_up(repo, expected, kept)?;
        Self::update_branch_ref(repo, branch, expected, to)
    }
}

/// The ref namespace savepoints live under.
const SAVEPOINT_NAMESPACE: &str = "refs/rwv/pre-op";

/// The ref directory every savepoint filed under `key` lives below: the
/// checkout's (`checkout/<branch>` or `detached`) and each branch's moved as
/// a ref (`ref/<branch>`).
///
/// The namespacing is a git impl detail — callers of the [`Vcs`] trait pass
/// a [`SavepointKey`] and reach the name, when they must show it to an
/// operator, through [`Vcs::savepoint_label`] or [`Vcs::savepoint_namespace`].
/// Centralizing the format here means create / resolve / drop / restore all
/// agree on the layout.
fn savepoint_ref(key: &SavepointKey) -> String {
    format!("{SAVEPOINT_NAMESPACE}/{key}")
}

/// The ref namespace backups live under.
const BACKUP_NAMESPACE: &str = "refs/rwv/backup";

impl GitVcs {
    /// Reset `repo` to `savepoint` via `git reset --hard`, drop the savepoint
    /// ref, and return `outcome`. This factors out the identical reset+drop
    /// sequence shared by the intent, converged, and mid-op restore branches
    /// of [`Vcs::verified_restore_savepoint`].
    fn reset_and_drop_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        savepoint: &ResolvedRevisionId,
        outcome: VerifiedRestoreOutcome,
        kept: &mut Vec<KeptWork>,
    ) -> Result<VerifiedRestoreOutcome, VcsError> {
        self.keep_before_discarding(repo, savepoint, kept)?;
        Self::run(["reset", "--hard", savepoint.id().as_str()], repo)?;
        self.drop_checkout_savepoint(repo, key)?;
        Ok(outcome)
    }

    /// Before a hard move of `repo`'s checkout to `to`: keep its uncommitted
    /// edits as a backup where the move would discard any (a tracked change,
    /// or an untracked file at a path `to` tracks), once per restore, and the
    /// commits a detached HEAD holds that no branch reaches.
    fn keep_before_discarding(
        &self,
        repo: &Path,
        to: &ResolvedRevisionId,
        kept: &mut Vec<KeptWork>,
    ) -> Result<(), VcsError> {
        if !kept.iter().any(|k| matches!(k, KeptWork::Edits(_)))
            && Self::hard_move_discards(repo, to)?
        {
            if let Some(edits) = self.backup_edits(repo)? {
                kept.push(KeptWork::Edits(edits));
            }
        }
        if let Some(commits) = self.backup_unpreserved_head(repo)? {
            kept.push(KeptWork::Commits(commits));
        }
        Ok(())
    }

    fn hard_move_discards(repo: &Path, to: &ResolvedRevisionId) -> Result<bool, VcsError> {
        if !Self::dirty_file_names_inner(repo, StatusReach::Tracked)?.is_empty() {
            return Ok(true);
        }
        let untracked = Self::run(["ls-files", "--others", "--exclude-standard", "-z"], repo)?;
        let untracked: BTreeSet<&str> = untracked.split('\0').filter(|p| !p.is_empty()).collect();
        if untracked.is_empty() {
            return Ok(false);
        }
        let tracked = Self::run(["ls-tree", "-r", "-z", "--name-only", to.id().as_str()], repo)?;
        Ok(tracked
            .split('\0')
            .any(|path| untracked.contains(path)))
    }

    fn restore_checkout_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        attribution: &Attribution<'_>,
        foreign_tip_policy: ForeignTipPolicy,
        kept: &mut Vec<KeptWork>,
    ) -> Result<VerifiedRestoreOutcome, VcsError> {
        let Some((on, savepoint)) = self.checkout_savepoint(repo, key)? else {
            return Ok(match self.mid_op(repo) {
                Some(stopped) => VerifiedRestoreOutcome::LeftStopped { stopped },
                None => VerifiedRestoreOutcome::NoSavepoint,
            });
        };
        let savepoint = ResolvedRevisionId::from(savepoint);
        let attributable = |tip: &CommitId| tip == savepoint.id() || attribution.wrote(tip);

        if let Some(stopped) = self.mid_op(repo) {
            let returns_to = self.cancel_returns_to(repo)?;
            let own = attribution.integrates == Integrates::Yes
                && returns_to.as_ref().is_some_and(|r| attributable(r.id()));
            let at_savepoint = returns_to.as_ref().map(|r| r.id()) == Some(savepoint.id());
            if !own {
                if attribution.integrates == Integrates::No && at_savepoint {
                    self.drop_checkout_savepoint(repo, key)?;
                    return Ok(VerifiedRestoreOutcome::LeftStopped { stopped });
                }
                let unread_but_at_savepoint =
                    attribution.integrates == Integrates::Unknown && at_savepoint;
                if !unread_but_at_savepoint && foreign_tip_policy == ForeignTipPolicy::Refuse {
                    return Ok(VerifiedRestoreOutcome::StoppedForeign {
                        stopped,
                        returns_to: returns_to.map_or_else(
                            || self.head_revision(repo).map(|h| h.id().clone()),
                            |r| Ok(r.id().clone()),
                        )?,
                        savepoint: savepoint.id().clone(),
                    });
                }
            }
            if !own {
                let to = match returns_to {
                    Some(to) => to,
                    None => self.head_revision(repo)?,
                };
                self.keep_before_discarding(repo, &to, kept)?;
            }
            self.cancel_in_flight_op(repo);
            if self.mid_op(repo).is_some() {
                return Err(VcsError::CommandFailed {
                    args: vec![stopped.to_string(), "--abort".to_owned()],
                    repo: repo.to_path_buf(),
                    stderr: format!("the {stopped} in progress could not be canceled"),
                });
            }
            let then =
                self.restore_checkout_savepoint(repo, key, attribution, foreign_tip_policy, kept)?;
            return Ok(VerifiedRestoreOutcome::Canceled {
                stopped,
                then: Box::new(then),
            });
        }

        let head = self.head_revision(repo)?;
        let head_id = head.id();

        let off = match (&on, self.observe_head(repo)?) {
            (SavedCheckout::Unnamed, _) => None,
            (SavedCheckout::On(b), HeadObservation::Attached { name }) if &name == b => None,
            (SavedCheckout::Detached, HeadObservation::Detached { .. }) => None,
            (_, HeadObservation::Attached { name }) => Some(Some(name)),
            (_, HeadObservation::Detached { .. } | HeadObservation::Unborn { .. }) => Some(None),
        };
        if let Some(off) = off {
            if !attributable(head_id) && foreign_tip_policy == ForeignTipPolicy::Refuse {
                return Ok(VerifiedRestoreOutcome::ForeignTip {
                    observed_tip: head_id.clone(),
                    savepoint: savepoint.id().clone(),
                    recorded_converged_tip: attribution.converged.cloned(),
                });
            }
            if !attributable(head_id) && off.is_none() {
                self.create_backup(
                    repo,
                    &BackupSource::DetachedHead,
                    &head,
                    Some(BackupMark::Abandoned),
                )?;
            }
            match &on {
                SavedCheckout::On(b) => self.attach_head_to(repo, &LocalRefName::of_branch(b))?,
                SavedCheckout::Detached | SavedCheckout::Unnamed => {
                    self.set_detached_head(repo, &savepoint)?
                }
            }
            let then =
                self.restore_checkout_savepoint(repo, key, attribution, foreign_tip_policy, kept)?;
            return Ok(VerifiedRestoreOutcome::SwitchedBack {
                off,
                then: Box::new(then),
            });
        }

        if head_id == savepoint.id() {
            self.drop_checkout_savepoint(repo, key)?;
            return Ok(VerifiedRestoreOutcome::Untouched);
        }

        if attribution.intent == Some(head_id) {
            return self.reset_and_drop_savepoint(
                repo,
                key,
                &savepoint,
                VerifiedRestoreOutcome::RestoredFromIntent,
                kept,
            );
        }

        if attribution.wrote(head_id) {
            return self.reset_and_drop_savepoint(
                repo,
                key,
                &savepoint,
                VerifiedRestoreOutcome::RestoredFromConverged,
                kept,
            );
        }

        if foreign_tip_policy == ForeignTipPolicy::Abandon {
            let source = self.head_backup_source(repo)?;
            let backup = self.create_backup(repo, &source, &head, Some(BackupMark::Abandoned))?;
            return self.reset_and_drop_savepoint(
                repo,
                key,
                &savepoint,
                VerifiedRestoreOutcome::AbandonedForeignTip {
                    abandoned_tip: head_id.clone(),
                    savepoint: savepoint.id().clone(),
                    backup,
                },
                kept,
            );
        }

        Ok(VerifiedRestoreOutcome::ForeignTip {
            observed_tip: head_id.clone(),
            savepoint: savepoint.id().clone(),
            recorded_converged_tip: attribution.converged.cloned(),
        })
    }

    /// Every savepoint filed under `key` in `repo`'s store.
    fn savepoints_under(repo: &Path, key: &SavepointKey) -> Result<Vec<FiledSavepoint>, VcsError> {
        let base = savepoint_ref(key);
        let listing = Self::run_trimmed(
            ["for-each-ref", "--format=%(objectname) %(refname)", &base],
            repo,
        )?;
        Ok(listing
            .lines()
            .filter_map(|line| {
                let (at, ref_name) = line.trim().split_once(' ')?;
                let saved = match ref_name.strip_prefix(&base)? {
                    "" => Saved::Checkout(SavedCheckout::Unnamed),
                    below => Saved::parse(below.strip_prefix('/')?)?,
                };
                Some(FiledSavepoint {
                    ref_name: ref_name.to_owned(),
                    saved,
                    at: CommitId::parse(at).ok()?,
                })
            })
            .collect())
    }

    /// The checkout savepoint filed under `key`: where the checkout was, and
    /// its tip.
    fn checkout_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
    ) -> Result<Option<(SavedCheckout, CommitId)>, VcsError> {
        Ok(Self::savepoints_under(repo, key)?
            .into_iter()
            .find_map(|filed| match filed.saved {
                Saved::Checkout(on) => Some((on, filed.at)),
                Saved::Ref(_) => None,
            }))
    }
}

/// Where a checkout was when its savepoint was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SavedCheckout {
    On(RawRefName),
    Detached,
    /// Filed by an earlier release at the key's own name, saying nothing of
    /// a branch. Only `rwv doctor --fix` meets one: the op record that
    /// owned it is in a schema this release refuses.
    Unnamed,
}

impl SavedCheckout {
    fn ref_name(&self, key: &SavepointKey) -> String {
        match self {
            Self::On(branch) => format!("{}/checkout/{}", savepoint_ref(key), branch.as_str()),
            Self::Detached => format!("{}/detached", savepoint_ref(key)),
            Self::Unnamed => savepoint_ref(key),
        }
    }
}

/// What one savepoint ref under a key saves.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Saved {
    Checkout(SavedCheckout),
    Ref(RawRefName),
}

impl Saved {
    /// Read the part of a savepoint ref's name below its key.
    fn parse(below_key: &str) -> Option<Self> {
        if below_key == "detached" {
            return Some(Self::Checkout(SavedCheckout::Detached));
        }
        if let Some(branch) = below_key.strip_prefix("checkout/") {
            return Some(Self::Checkout(SavedCheckout::On(RawRefName::new(branch))));
        }
        below_key
            .strip_prefix("ref/")
            .map(|branch| Self::Ref(RawRefName::new(branch)))
    }
}

struct FiledSavepoint {
    ref_name: String,
    saved: Saved,
    at: CommitId,
}

fn ref_savepoint_ref(key: &SavepointKey, branch: &RawRefName) -> String {
    format!("{}/ref/{}", savepoint_ref(key), branch.as_str())
}

/// One record of `git worktree list --porcelain`.
struct WorktreeRecord {
    path: PathBuf,
    branch: Option<RawRefName>,
    /// git marks a registration prunable when it no longer leads to a
    /// worktree, unless it is locked.
    prunable: bool,
    locked: bool,
}

impl GitVcs {
    fn worktree_records(repo: &Path) -> Result<Vec<WorktreeRecord>, VcsError> {
        let output = Self::run_trimmed_awaiting_peers(["worktree", "list", "--porcelain"], repo)?;
        let mut records = Vec::new();
        for block in output.split("\n\n") {
            let mut record = WorktreeRecord {
                path: PathBuf::new(),
                branch: None,
                prunable: false,
                locked: false,
            };
            for line in block.lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    record.path = PathBuf::from(path);
                } else if let Some(name) = line
                    .strip_prefix("branch ")
                    .and_then(|full| full.strip_prefix(LOCAL_BRANCH_NAMESPACE))
                {
                    record.branch = Some(RawRefName::new(name));
                } else if line == "prunable" || line.starts_with("prunable ") {
                    record.prunable = true;
                } else if line == "locked" || line.starts_with("locked ") {
                    record.locked = true;
                }
            }
            if !record.path.as_os_str().is_empty() {
                records.push(record);
            }
        }
        Ok(records)
    }
}

/// Whether `registered` and `asked` name one place, though one is spelled
/// through a symlinked or relative parent. Only the parents resolve: the
/// leaf of a stale registration no longer exists.
fn same_location(registered: &Path, asked: &Path) -> bool {
    let resolved = |path: &Path| {
        let parent = crate::path_spelling::resolve(path.parent()?).ok()?;
        Some(parent.join(path.file_name()?))
    };
    registered == asked || resolved(registered).is_some_and(|r| Some(r) == resolved(asked))
}

/// How far past tracked files a `git status` read looks.
#[derive(Clone, Copy)]
enum StatusReach {
    Tracked,
    Untracked,
    Ignored,
}

/// One record of `git status --porcelain -z` v1.
struct PorcelainEntry {
    /// Index column. `' '` when the index matches HEAD, `'?'` for a file git
    /// is not tracking.
    staged: u8,
    /// Working-tree column. `' '` when the working file matches the index.
    worktree: u8,
    /// The path the entry describes; for a rename or copy, the name after it.
    path: String,
    /// The name a rename moved `path` from, which the rename leaves changed
    /// too. A copy's source is unchanged and is not carried.
    renamed_from: Option<String>,
}

impl GitVcs {
    /// Read `git status --porcelain -z`, preserving the status columns.
    ///
    /// Decoded lossily rather than through [`GitVcs::run`], which refuses
    /// output that is not UTF-8: a changed path git cannot spell in UTF-8
    /// still leaves the tree dirty, and that answer must not become an error.
    fn porcelain_status(repo: &Path, reach: StatusReach) -> Result<Vec<PorcelainEntry>, VcsError> {
        let mut args = vec!["status", "--porcelain", "-z"];
        match reach {
            StatusReach::Tracked => args.push("--untracked-files=no"),
            StatusReach::Untracked => {}
            StatusReach::Ignored => args.push("--ignored"),
        }
        let out = git_command()
            .args(&args)
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        if !out.status.success() {
            return Err(VcsError::CommandFailed {
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                repo: repo.to_path_buf(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut fields = stdout.split('\0').filter(|field| !field.is_empty());
        let mut entries = Vec::new();
        // `XY<space>path`, and for a rename or copy (`R` or `C` in either
        // column) the source name follows as the next field.
        while let Some(record) = fields.next() {
            let (Some(columns), Some(path)) = (record.get(..2), record.get(3..)) else {
                continue;
            };
            let source = if columns.contains(['R', 'C']) {
                fields.next()
            } else {
                None
            };
            entries.push(PorcelainEntry {
                staged: columns.as_bytes()[0],
                worktree: columns.as_bytes()[1],
                path: path.to_owned(),
                renamed_from: source.filter(|_| columns.contains('R')).map(str::to_owned),
            });
        }
        Ok(entries)
    }

    /// Every path `git status --porcelain` reports, with a rename naming
    /// both its source and its destination.
    fn dirty_file_names_inner(repo: &Path, reach: StatusReach) -> Result<Vec<String>, VcsError> {
        Ok(Self::porcelain_status(repo, reach)?
            .into_iter()
            .flat_map(|entry| std::iter::once(entry.path).chain(entry.renamed_from))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Population
// ---------------------------------------------------------------------------

/// Whether `worktree add` writes the checkout's files.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorktreeFiles {
    Written,
    Unwritten,
}

/// The oldest release every operation of this backend runs on, outside the
/// capabilities it detects. `git hook run`, which finishing a populated
/// workspace needs, arrived in 2.36.
const MINIMUM_RELEASE: (u32, u32) = (2, 36);

/// `merge-tree --write-tree`, which computes a merge as a tree, arrived in 2.38.
const FIRST_RELEASE_MERGING_TREES: (u32, u32) = (2, 38);

/// What `git version` answered, read once per process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GitReading {
    Release {
        release: (u32, u32),
        version: String,
    },
    Unreadable {
        reported: String,
    },
    Unrunnable {
        cause: String,
    },
}

fn git_reading() -> &'static GitReading {
    static READING: std::sync::OnceLock<GitReading> = std::sync::OnceLock::new();
    READING.get_or_init(|| match git_command().args(["version"]).output() {
        Err(e) => GitReading::Unrunnable {
            cause: format!("{e:#}"),
        },
        Ok(out) => reading_of(
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
    })
}

fn reading_of(succeeded: bool, stdout: &str, stderr: &str) -> GitReading {
    match release_of(stdout).filter(|_| succeeded) {
        Some(release) => GitReading::Release {
            release,
            version: stdout.trim().trim_start_matches("git version ").to_owned(),
        },
        None => GitReading::Unreadable {
            reported: [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        },
    }
}

fn readiness_of(reading: &GitReading) -> ToolReadiness {
    match reading {
        GitReading::Release { release, .. } if *release >= MINIMUM_RELEASE => ToolReadiness::Ready,
        GitReading::Release { version, .. } => ToolReadiness::TooOld {
            installed: version.clone(),
        },
        GitReading::Unreadable { reported } => ToolReadiness::Unreadable {
            reported: reported.clone(),
        },
        GitReading::Unrunnable { cause } => ToolReadiness::Unrunnable {
            cause: cause.clone(),
        },
    }
}

fn git_merges_trees() -> bool {
    matches!(git_reading(), GitReading::Release { release, .. } if *release >= FIRST_RELEASE_MERGING_TREES)
}

/// `(major, minor)` from `git version`, whose tail vendors extend freely
/// (`2.39.5 (Apple Git-154)`, `2.47.1.windows.1`).
fn release_of(version_output: &str) -> Option<(u32, u32)> {
    let mut numbers = version_output
        .trim()
        .strip_prefix("git version ")?
        .split('.')
        .map(str::parse::<u32>);
    Some((numbers.next()?.ok()?, numbers.next()?.ok()?))
}

/// Every attribute git's checkout consults when it writes a file's bytes.
const CONVERSION_ATTRIBUTES: [&str; 6] = [
    "text",
    "eol",
    "crlf",
    "ident",
    "filter",
    "working-tree-encoding",
];

/// A path's [`CONVERSION_ATTRIBUTES`] as `check-attr` prints them, in that
/// order: `set`, `unset`, `unspecified`, or the value.
type ConversionAttributes = [String; CONVERSION_ATTRIBUTES.len()];

/// The configuration git's checkout consults for line endings, as written.
#[derive(Debug, Default)]
struct ConversionConfig {
    autocrlf: Option<String>,
    eol: Option<String>,
}

impl ConversionConfig {
    /// Whether `text` content checks out with LF line endings. A value git
    /// would read differently from how it is matched here counts as CRLF.
    fn text_checks_out_lf(&self) -> bool {
        match self.autocrlf.as_deref() {
            Some(v) if v.eq_ignore_ascii_case("input") => true,
            None => self.eol_is_lf(),
            Some(v) if v.eq_ignore_ascii_case("false") => self.eol_is_lf(),
            Some(_) => false,
        }
    }

    /// Whether content no attribute calls text checks out unconverted.
    fn undeclared_checks_out_unconverted(&self) -> bool {
        match self.autocrlf.as_deref() {
            None => true,
            Some(v) => v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("input"),
        }
    }

    fn eol_is_lf(&self) -> bool {
        match self.eol.as_deref() {
            Some(v) if v.eq_ignore_ascii_case("lf") => true,
            None => !cfg!(windows),
            Some(v) if v.eq_ignore_ascii_case("native") => !cfg!(windows),
            Some(_) => false,
        }
    }
}

/// How an attribute value directs end-of-line conversion.
enum EolAction {
    Binary,
    Text,
    TextInput,
    Auto,
    Undeclared,
}

fn eol_action(value: &str) -> Option<EolAction> {
    match value {
        "set" => Some(EolAction::Text),
        "unset" => Some(EolAction::Binary),
        "input" => Some(EolAction::TextInput),
        "auto" => Some(EolAction::Auto),
        "unspecified" => Some(EolAction::Undeclared),
        _ => None,
    }
}

/// Whether git's checkout writes a blob's bytes unchanged under these
/// inputs, whatever the blob holds.
///
/// Mirrors git's own end-of-line decision, so it must never answer `true`
/// where git converts: placing the blob's bytes there would differ from the
/// checkout, and an unfiltered hash would still call the placed file right.
/// Every value it does not recognize answers `false`.
fn checks_out_verbatim(attrs: &ConversionAttributes, config: &ConversionConfig) -> bool {
    let [text, eol, crlf, ident, filter, encoding] = attrs.each_ref().map(String::as_str);
    if !matches!(filter, "unspecified" | "unset")
        || !matches!(ident, "unspecified" | "unset")
        || encoding != "unspecified"
    {
        return false;
    }
    let declared = if text == "unspecified" { crlf } else { text };
    let Some(action) = eol_action(declared) else {
        return false;
    };
    if matches!(action, EolAction::Binary) {
        return true;
    }
    match eol {
        "lf" => true,
        "unspecified" => match action {
            EolAction::Binary | EolAction::TextInput => true,
            EolAction::Text | EolAction::Auto => config.text_checks_out_lf(),
            EolAction::Undeclared => config.undeclared_checks_out_unconverted(),
        },
        _ => false,
    }
}

fn content_identity(
    blob: &str,
    attrs: &ConversionAttributes,
    config: &ConversionConfig,
) -> ContentIdentity {
    ContentIdentity::new(format!(
        "{blob} {attrs:?} {:?} {:?}",
        config.autocrlf, config.eol
    ))
}

/// How git records a path in the index and in a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileMode {
    Regular,
    Executable,
    Symlink,
    /// A submodule's commit.
    Gitlink,
}

impl FileMode {
    /// The mode named by git's octal spelling, or `None` for one git does not
    /// record a path under in an index.
    fn parse(token: &str) -> Option<Self> {
        match token {
            "100644" => Some(Self::Regular),
            "100755" => Some(Self::Executable),
            "120000" => Some(Self::Symlink),
            "160000" => Some(Self::Gitlink),
            _ => None,
        }
    }

    fn is_regular_file(self) -> bool {
        matches!(self, Self::Regular | Self::Executable)
    }
}

/// One merged index entry.
struct IndexEntry {
    mode: FileMode,
    blob: String,
    path: String,
}

impl IndexEntry {
    fn is_regular_file(&self) -> bool {
        self.mode.is_regular_file()
    }
}

/// `--stdin-paths` reads a line per path, drops a CR before the newline, and
/// C-unquotes a line opening with `"`; a path any of that would alter is
/// sent quoted.
fn as_stdin_path_line(path: &str) -> Cow<'_, str> {
    if !(path.starts_with('"') || path.contains('\n') || path.ends_with('\r')) {
        return Cow::Borrowed(path);
    }
    let mut quoted = String::from("\"");
    for c in path.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    Cow::Owned(quoted)
}

fn nul_terminated<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    paths
        .into_iter()
        .flat_map(|p| p.bytes().chain([0]))
        .collect()
}

#[cfg(unix)]
fn executable_bit_matches(meta: &std::fs::Metadata, executable: bool) -> bool {
    use std::os::unix::fs::PermissionsExt;
    (meta.permissions().mode() & 0o100 != 0) == executable
}

/// git's checkout writes no executable bit where the filesystem has none.
#[cfg(not(unix))]
fn executable_bit_matches(_meta: &std::fs::Metadata, _executable: bool) -> bool {
    true
}

impl GitVcs {
    fn add_worktree(
        &self,
        store: &Path,
        dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
        files: WorktreeFiles,
    ) -> Result<RefOrigin, VcsError> {
        let store = Self::work_dir_for_store(store);
        let dest_arg = GitArg::from_path(dest, "worktree path")?;
        let start = start_point.id().as_str();
        let branch = name.as_str();
        // `--no-checkout` runs no `post-checkout` hook, so a failure after
        // registration is a hook rejection only when files are written.
        let registered_before =
            (files == WorktreeFiles::Written).then(|| self.is_registered_worktree(&store, dest));
        let classify = |e| match registered_before {
            Some(before) => self.classify_worktree_add_failure(e, &store, dest, before),
            None => e,
        };
        let add: &[&str] = match files {
            WorktreeFiles::Written => &["worktree", "add"],
            WorktreeFiles::Unwritten => &["worktree", "add", "--no-checkout"],
        };

        // Classify BEFORE acting, by asking whether the ref is there. Two
        // reasons it cannot be done by matching stderr afterwards: git says
        // "already exists" for a taken destination path as well as for a
        // taken branch name, and `worktree add -b` creates the branch
        // before it fails on the destination, so a post-hoc look sees a ref
        // this very call just made.
        if self.resolve_local_branch_tip(&store, name)?.is_some() {
            // ADOPT. The branch is checked out as it stands, never deleted
            // and made again: deleting a ref takes a `DestroyProof`, and a
            // name match is not one.
            Self::add_awaiting_peers(Argv::from(add).path(&dest_arg).text(branch), &store, dest)
                .map_err(classify)?;
            return Ok(RefOrigin::Adopted);
        }
        // AUTHOR. If this fails after git has already written the ref (a
        // taken destination is the usual way), the ref is left in place:
        // deleting it takes a `DestroyProof`, which this call does not hold.
        //
        // git creates the branch before it reads the registrations, so an
        // attempt that dies on a peer's has already authored it: the next one
        // checks the branch out instead of creating it, and the call is still
        // the one that authored it.
        Self::awaiting_peer_registration(&store, Some(dest), UNWRITTEN_REGISTRATION_WAIT, || {
            let created = self.resolve_local_branch_tip(&store, name)?.is_some();
            let argv = if created {
                Argv::from(add).path(&dest_arg).text(branch)
            } else {
                Argv::from(add)
                    .text("-b")
                    .text(branch)
                    .path(&dest_arg)
                    .text(start)
            };
            Self::run(argv, &store)
        })
        .map_err(classify)?;
        Ok(RefOrigin::Authored)
    }

    fn add_detached_worktree(
        &self,
        store: &Path,
        dest: &Path,
        at: &ResolvedRevisionId,
        files: WorktreeFiles,
    ) -> Result<(), VcsError> {
        let store = Self::work_dir_for_store(store);
        let dest_arg = GitArg::from_path(dest, "worktree path")?;
        let registered_before =
            (files == WorktreeFiles::Written).then(|| self.is_registered_worktree(&store, dest));
        let add: &[&str] = match files {
            WorktreeFiles::Written => &["worktree", "add", "--detach"],
            WorktreeFiles::Unwritten => &["worktree", "add", "--detach", "--no-checkout"],
        };
        Self::awaiting_peer_registration(&store, Some(dest), UNWRITTEN_REGISTRATION_WAIT, || {
            Self::run(
                Argv::from(add).path(&dest_arg).text(at.id().as_str()),
                &store,
            )
        })
        .map_err(|e| match registered_before {
            Some(before) => self.classify_worktree_add_failure(e, &store, dest, before),
            None => e,
        })?;
        Ok(())
    }

    /// [`GitVcs::run`] with `input` on stdin.
    fn run_with_stdin(args: impl Into<Argv>, dir: &Path, input: &[u8]) -> Result<String, VcsError> {
        let args = args.into();
        let mut child = git_command()
            .args(&args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let (written, output) = std::thread::scope(|s| {
            let writer = s.spawn(move || stdin.write_all(input));
            let output = child.wait_with_output();
            (writer.join().expect("stdin writer panicked"), output)
        });
        let output = output.map_err(|e| VcsError::Io {
            ctx: format!("failed to read git {args:?}"),
            source: e,
        })?;
        if !output.status.success() {
            return Err(VcsError::CommandFailed {
                args: args.into_words(),
                repo: dir.to_path_buf(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        written.map_err(|e| VcsError::Io {
            ctx: format!("failed to feed git {args:?}"),
            source: e,
        })?;
        String::from_utf8(output.stdout).map_err(|_| VcsError::CommandFailed {
            args: args.into_words(),
            repo: dir.to_path_buf(),
            stderr: "git output not valid UTF-8".to_string(),
        })
    }

    /// The objects among `ids` the repository at `repo` holds, of any type.
    fn objects_held<'a>(
        repo: &Path,
        ids: &BTreeSet<&'a str>,
    ) -> Result<BTreeSet<&'a str>, VcsError> {
        if ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let input: String = ids.iter().map(|id| format!("{id}\n")).collect();
        let report = Self::run_with_stdin(["cat-file", "--batch-check"], repo, input.as_bytes())?;
        let missing: BTreeSet<&str> = report
            .lines()
            .filter_map(|line| line.strip_suffix(" missing"))
            .collect();
        Ok(ids
            .iter()
            .copied()
            .filter(|id| !missing.contains(id))
            .collect())
    }

    fn index_entries(workspace: &Path) -> Result<Vec<IndexEntry>, VcsError> {
        let listing = Self::run_with_stdin(["ls-files", "--stage", "-z"], workspace, &[])?;
        let malformed = |record: &str| VcsError::CommandFailed {
            args: vec!["ls-files".to_owned(), "--stage".to_owned(), "-z".to_owned()],
            repo: workspace.to_path_buf(),
            stderr: format!("unrecognized index entry {record:?}"),
        };
        let mut entries = Vec::new();
        for record in listing.split_terminator('\0') {
            let (meta, path) = record.split_once('\t').ok_or_else(|| malformed(record))?;
            let [mode, blob, stage] = meta
                .split(' ')
                .collect::<Vec<_>>()
                .try_into()
                .map_err(|_| malformed(record))?;
            if stage == "0" {
                let mode = FileMode::parse(mode).ok_or_else(|| VcsError::CommandFailed {
                    args: vec!["ls-files".to_owned(), "--stage".to_owned(), "-z".to_owned()],
                    repo: workspace.to_path_buf(),
                    stderr: format!("unrecognized mode {mode} for {path}"),
                })?;
                entries.push(IndexEntry {
                    mode,
                    blob: blob.to_owned(),
                    path: path.to_owned(),
                });
            }
        }
        Ok(entries)
    }

    fn conversion_attributes(
        workspace: &Path,
        paths: &[&str],
    ) -> Result<HashMap<String, ConversionAttributes>, VcsError> {
        let mut args = vec!["check-attr", "--cached", "-z", "--stdin"];
        args.extend(CONVERSION_ATTRIBUTES);
        let report =
            Self::run_with_stdin(&args, workspace, &nul_terminated(paths.iter().copied()))?;
        let mut by_path: HashMap<String, ConversionAttributes> = HashMap::new();
        let fields: Vec<&str> = report.split_terminator('\0').collect();
        for triple in fields.chunks(3) {
            let [path, attr, value] = triple else {
                return Err(VcsError::CommandFailed {
                    args: args.iter().map(|s| (*s).to_owned()).collect(),
                    repo: workspace.to_path_buf(),
                    stderr: format!("unrecognized check-attr record {triple:?}"),
                });
            };
            if let Some(slot) = CONVERSION_ATTRIBUTES.iter().position(|a| a == attr) {
                by_path.entry((*path).to_owned()).or_default()[slot] = (*value).to_owned();
            }
        }
        Ok(by_path)
    }

    fn conversion_config(workspace: &Path) -> Result<ConversionConfig, VcsError> {
        let args = ["config", "--get-regexp", r"^core\.(autocrlf|eol)$"];
        let out = git_command()
            .args(args)
            .current_dir(workspace)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        // `--get-regexp` exits 1 when no key matches.
        let found = match out.status.code() {
            Some(0) => String::from_utf8_lossy(&out.stdout).into_owned(),
            Some(1) if out.stdout.is_empty() => String::new(),
            _ => {
                return Err(VcsError::CommandFailed {
                    args: args.iter().map(|s| (*s).to_owned()).collect(),
                    repo: workspace.to_path_buf(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                })
            }
        };
        let mut config = ConversionConfig::default();
        for line in found.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "core.autocrlf" => config.autocrlf = Some(value.to_owned()),
                "core.eol" => config.eol = Some(value.to_owned()),
                _ => {}
            }
        }
        Ok(config)
    }

    fn describe(workspace: &Path, entries: &[IndexEntry]) -> Result<Vec<TrackedFile>, VcsError> {
        let files: Vec<&str> = entries
            .iter()
            .filter(|e| e.is_regular_file())
            .map(|e| e.path.as_str())
            .collect();
        let (attrs, config) = if files.is_empty() {
            (HashMap::new(), ConversionConfig::default())
        } else {
            (
                Self::conversion_attributes(workspace, &files)?,
                Self::conversion_config(workspace)?,
            )
        };
        entries
            .iter()
            .map(|entry| {
                let identity = || {
                    let attrs = attrs.get(&entry.path)?;
                    checks_out_verbatim(attrs, &config)
                        .then(|| content_identity(&entry.blob, attrs, &config))
                };
                let kind = match entry.mode {
                    FileMode::Regular => TrackedFileKind::File {
                        executable: false,
                        identity: identity(),
                    },
                    FileMode::Executable => TrackedFileKind::File {
                        executable: true,
                        identity: identity(),
                    },
                    FileMode::Symlink => TrackedFileKind::Symlink,
                    FileMode::Gitlink => TrackedFileKind::NestedRepository,
                };
                Ok(TrackedFile {
                    path: entry.path.clone(),
                    kind,
                })
            })
            .collect()
    }

    /// The object id each file's bytes would have, with no conversion.
    fn unfiltered_hashes(workspace: &Path, paths: &[&str]) -> Result<Vec<String>, VcsError> {
        let input: String = paths
            .iter()
            .map(|p| format!("{}\n", as_stdin_path_line(p)))
            .collect();
        let args = ["hash-object", "--no-filters", "--stdin-paths"];
        let hashes: Vec<String> = Self::run_with_stdin(args, workspace, input.as_bytes())?
            .lines()
            .map(str::to_owned)
            .collect();
        if hashes.len() != paths.len() {
            return Err(VcsError::CommandFailed {
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                repo: workspace.to_path_buf(),
                stderr: format!("{} hashes for {} paths", hashes.len(), paths.len()),
            });
        }
        Ok(hashes)
    }

    fn run_post_checkout_hook(workspace: &Path) -> Result<(), VcsError> {
        let head = Self::run_trimmed(["rev-parse", "--verify", "HEAD"], workspace)?;
        let no_previous_head = "0".repeat(head.len());
        let args = [
            "hook",
            "run",
            "--ignore-missing",
            "post-checkout",
            "--",
            &no_previous_head,
            &head,
            "1",
        ];
        let out = git_command()
            .args(args)
            .current_dir(workspace)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        if out.status.success() {
            return Ok(());
        }
        Err(VcsError::HookRejected {
            repo: workspace.to_path_buf(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

impl WorkspacePopulation for GitVcs {
    fn materialize_unpopulated_worktree_on_ref(
        &self,
        store: &Path,
        dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<RefOrigin, VcsError> {
        let ref_origin =
            self.add_worktree(store, dest, name, start_point, WorktreeFiles::Unwritten)?;
        // `--no-checkout` leaves no index; reading the tree into one is what
        // makes the workspace record its revision before any file exists.
        Self::run(["read-tree", "HEAD"], dest)?;
        Ok(ref_origin)
    }

    fn materialize_unpopulated_worktree_detached(
        &self,
        store: &Path,
        dest: &Path,
        at: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        self.add_detached_worktree(store, dest, at, WorktreeFiles::Unwritten)?;
        Self::run(["read-tree", "HEAD"], dest)?;
        Ok(())
    }

    fn tracked_files(&self, workspace: &Path) -> Result<Vec<TrackedFile>, VcsError> {
        Self::describe(workspace, &Self::index_entries(workspace)?)
    }

    fn files_holding_recorded_content(
        &self,
        workspace: &Path,
        paths: &[String],
    ) -> Result<Vec<TrackedFile>, VcsError> {
        let asked: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
        let entries: Vec<IndexEntry> = Self::index_entries(workspace)?
            .into_iter()
            .filter(|e| asked.contains(e.path.as_str()))
            .collect();
        let candidates: Vec<(String, TrackedFile)> = entries
            .iter()
            .zip(Self::describe(workspace, &entries)?)
            .filter(|(_, file)| match &file.kind {
                TrackedFileKind::File {
                    executable,
                    identity: Some(_),
                } => std::fs::symlink_metadata(workspace.join(&file.path)).is_ok_and(|meta| {
                    meta.file_type().is_file() && executable_bit_matches(&meta, *executable)
                }),
                _ => false,
            })
            .map(|(entry, file)| (entry.blob.clone(), file))
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let paths: Vec<&str> = candidates.iter().map(|(_, f)| f.path.as_str()).collect();
        let hashes = Self::unfiltered_hashes(workspace, &paths)?;
        Ok(candidates
            .into_iter()
            .zip(hashes)
            .filter(|((blob, _), hash)| blob == hash)
            .map(|((_, file), _)| file)
            .collect())
    }

    fn finish_checkout(&self, workspace: &Path) -> Result<(), VcsError> {
        // Only the missing paths are named: `checkout-index -a` exits 1 for
        // every file already present, so its status cannot tell a placed
        // file from a failed write.
        let missing: Vec<String> = Self::index_entries(workspace)?
            .into_iter()
            .map(|e| e.path)
            .filter(|p| std::fs::symlink_metadata(workspace.join(p)).is_err())
            .collect();
        if !missing.is_empty() {
            Self::run_with_stdin(
                ["checkout-index", "-u", "-z", "--stdin"],
                workspace,
                &nul_terminated(missing.iter().map(String::as_str)),
            )?;
        }
        Self::run(["update-index", "-q", "--refresh"], workspace)?;
        Self::run_post_checkout_hook(workspace)
    }

    fn check_out_paths(&self, workspace: &Path, paths: &[String]) -> Result<(), VcsError> {
        if paths.is_empty() {
            return Ok(());
        }
        Self::run_with_stdin(
            ["checkout-index", "-f", "-u", "-z", "--stdin"],
            workspace,
            &nul_terminated(paths.iter().map(String::as_str)),
        )
        .map(|_| ())
    }

    fn updates_replace_files(&self) -> bool {
        // Declared where tests/vcs/shared_file_conformance_test.rs holds the
        // property; a platform joins when its CI leg shows it green.
        cfg!(target_os = "linux")
    }

    fn recorded_submodules(&self, workspace: &Path) -> Result<Vec<RecordedSubmodule>, VcsError> {
        let gitlinks: Vec<IndexEntry> = Self::index_entries(workspace)?
            .into_iter()
            .filter(|e| e.mode == FileMode::Gitlink)
            .collect();
        if gitlinks.is_empty() {
            return Ok(Vec::new());
        }
        let names: HashMap<String, String> = Self::submodule_settings(workspace, true, "path")?
            .into_iter()
            .map(|(name, path)| (path, name))
            .collect();
        let mut updates = Self::submodule_settings(workspace, true, "update")?;
        updates.extend(Self::submodule_settings(workspace, false, "update")?);
        gitlinks
            .into_iter()
            .filter_map(|entry| {
                let name = names.get(&entry.path)?;
                is_confined_submodule_name(name).then(|| (name.clone(), entry))
            })
            .map(|(name, entry)| {
                let checkout = match updates.get(&name).map(String::as_str) {
                    Some("none") => SubmoduleCheckout::Declined,
                    _ => SubmoduleCheckout::Wanted,
                };
                Ok(RecordedSubmodule {
                    commit: commit_id_printed(
                        &entry.blob,
                        &["ls-files", "--stage", "-z"],
                        workspace,
                    )?,
                    name,
                    path: entry.path,
                    checkout,
                })
            })
            .collect()
    }

    fn module_data_holds(&self, module_data: &Path, submodule: &RecordedSubmodule) -> bool {
        let source = module_repository(module_data, submodule);
        let commit = format!("{}^{{commit}}", submodule.commit);
        // A partial clone fetches a missing object from its promisor remote
        // on read; refusing every transport makes the absence the answer.
        source.is_dir()
            && Self::run(
                ["-c", "protocol.allow=never", "cat-file", "-e", &commit],
                &source,
            )
            .is_ok()
    }

    fn create_unpopulated_submodule(
        &self,
        workspace: &Path,
        submodule: &RecordedSubmodule,
        module_data: &Path,
    ) -> Result<SubmoduleBirth, VcsError> {
        let repository = Self::submodule_repository(workspace, submodule)?;
        if repository.exists() {
            return Ok(SubmoduleBirth::RepositoryPresent {
                existing: repository,
            });
        }
        if !self.module_data_holds(module_data, submodule) {
            return Ok(SubmoduleBirth::NotLocal);
        }
        let source = module_repository(module_data, submodule);
        let dest = workspace.join(&submodule.path);
        for dir in [Some(dest.as_path()), repository.parent()]
            .into_iter()
            .flatten()
        {
            std::fs::create_dir_all(dir).map_err(|e| VcsError::Io {
                ctx: format!("failed to create {}", dir.display()),
                source: e,
            })?;
        }
        let repository_arg = GitArg::from_path(&repository, "submodule repository")?;
        let source_arg = GitArg::from_path(&source, "module data")?;
        let dest_arg = GitArg::from_path(&dest, "submodule path")?;
        // A path that names a repository is cloned by copying its objects
        // (as hard links where it can), never through a transport.
        Self::run(
            Argv::from(["clone", "--quiet", "--no-checkout", "--separate-git-dir"])
                .path(&repository_arg)
                .path(&source_arg)
                .path(&dest_arg),
            workspace,
        )?;
        Self::run(
            Argv::from(["config", "core.worktree"]).path(&dest_arg),
            &dest,
        )?;
        Self::run(
            [
                "update-ref",
                "--no-deref",
                "HEAD",
                submodule.commit.as_str(),
            ],
            &dest,
        )?;
        Self::run(["read-tree", "HEAD"], &dest)?;
        Ok(SubmoduleBirth::Born { source })
    }

    fn connect_submodule_upstream(
        &self,
        workspace: &Path,
        submodule: &RecordedSubmodule,
        source: &Path,
    ) -> Result<(), VcsError> {
        let Some(remotes) = Self::run_or_absent(
            ["config", "--local", "-z", "--get-regexp", r"^remote\."],
            source,
            Absent::ConfigKey,
        )?
        else {
            return Ok(());
        };
        let dest = workspace.join(&submodule.path);
        let cloned_remote = Self::only_remote(&dest)?;
        Self::run(
            [
                "config",
                "--remove-section",
                &format!("remote.{}", cloned_remote.name()),
            ],
            &dest,
        )?;
        for record in remotes.split_terminator('\0') {
            let (key, value) = record.split_once('\n').unwrap_or((record, ""));
            Self::run(["config", "--add", key, value], &dest)?;
        }
        Ok(())
    }

    fn discard_submodule(
        &self,
        workspace: &Path,
        submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError> {
        let repository = Self::submodule_repository(workspace, submodule)?;
        let dest = workspace.join(&submodule.path);
        let io = |what: &Path| {
            let what = what.to_path_buf();
            move |e| VcsError::Io {
                ctx: format!("failed to remove {}", what.display()),
                source: e,
            }
        };
        crate::workweave::remove_tree_outlasting_child_handles(&dest).map_err(io(&dest))?;
        std::fs::create_dir_all(&dest).map_err(io(&dest))?;
        if repository.exists() {
            crate::workweave::remove_tree_outlasting_child_handles(&repository)
                .map_err(io(&repository))?;
        }
        Ok(())
    }

    fn check_out_submodule_from_upstream(
        &self,
        workspace: &Path,
        submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError> {
        Self::run(
            Argv::from([
                "submodule",
                "--quiet",
                "update",
                "--init",
                "--recursive",
                "--",
            ])
            .text(&submodule.path),
            workspace,
        )
        .map(|_| ())
    }

    fn fetch_into_module_data(
        &self,
        module_data: &Path,
        submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError> {
        let source = module_repository(module_data, submodule);
        Self::run(["fetch", "--quiet"], &source)?;
        if self.module_data_holds(module_data, submodule) {
            return Ok(());
        }
        // A commit no branch or tag of the remote reaches is fetched by name,
        // as `git submodule update` does, from whichever remote has it.
        let mut last = None;
        for remote in Self::remote_names(&source)? {
            match Self::run(
                [
                    "fetch",
                    "--quiet",
                    remote.as_str(),
                    submodule.commit.as_str(),
                ],
                &source,
            ) {
                Ok(_) => return Ok(()),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or(VcsError::CommandFailed {
            args: vec!["fetch".into(), submodule.commit.as_str().to_owned()],
            repo: source,
            stderr: "the submodule's repository has no remote to fetch from".into(),
        }))
    }
}

/// The copy of `submodule`'s repository kept under `module_data`.
fn module_repository(module_data: &Path, submodule: &RecordedSubmodule) -> PathBuf {
    module_data.join("modules").join(&submodule.name)
}

/// Whether a `.gitmodules` name stays inside the `modules/` directory it is
/// joined under. git refuses a `..` component for the same reason; a name
/// rooted or carrying a drive or stream separator would leave it outright.
fn is_confined_submodule_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(':')
        && name
            .split(['/', '\\'])
            .all(|part| !part.is_empty() && part != "..")
}

impl GitVcs {
    /// `submodule path` → `submodule name`, from the checkout's `.gitmodules`.
    /// `submodule.<name>.<setting>` by name, from `.gitmodules` or, where git
    /// lets the repository's own config override it, from that.
    fn submodule_settings(
        workspace: &Path,
        from_gitmodules: bool,
        setting: &str,
    ) -> Result<HashMap<String, String>, VcsError> {
        if from_gitmodules && !workspace.join(".gitmodules").is_file() {
            return Ok(HashMap::new());
        }
        let pattern = format!(r"^submodule\..*\.{setting}$");
        let mut args = vec!["config"];
        if from_gitmodules {
            args.extend(["--file", ".gitmodules"]);
        }
        args.extend(["-z", "--get-regexp", &pattern]);
        let Some(listing) = Self::run_or_absent(args, workspace, Absent::ConfigKey)? else {
            return Ok(HashMap::new());
        };
        let suffix = format!(".{setting}");
        Ok(listing
            .split_terminator('\0')
            .filter_map(|record| {
                let (key, value) = record.split_once('\n')?;
                let name = key
                    .strip_prefix("submodule.")?
                    .strip_suffix(suffix.as_str())?;
                Some((name.to_owned(), value.to_owned()))
            })
            .collect())
    }

    /// Where `workspace` keeps the repository of `submodule`, whether or not
    /// one is there: git's own `modules/<name>` under the checkout's private
    /// git directory, so a linked worktree's never shares the main
    /// checkout's.
    fn submodule_repository(
        workspace: &Path,
        submodule: &RecordedSubmodule,
    ) -> Result<PathBuf, VcsError> {
        let git_path = format!("modules/{}", submodule.name);
        Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &git_path,
            ],
            workspace,
        )
        .map(PathBuf::from)
    }
}

#[cfg(test)]
mod branch_model_tests {
    //! Proofs, and consent-gated attachment changes.
    //!
    //! These live in-crate because the types they exercise are minted by
    //! crate-internal producers: a destroy proof by a check that runs here, a
    //! consent token from the flag module. That is the point — an
    //! integration test *cannot* forge them, which is the invariant.
    //!
    //! Everything reachable from outside the crate is in
    //! `tests/vcs/branch_model_test.rs`.

    use super::*;
    use crate::cli::consent::{DiscardUnlandedConsent, OverwriteConsent};
    use crate::manifest::{RepoOperand, RepoPath};
    use crate::op_state::OpId;
    use crate::vcs::{DestroyProof, FoundBranch, RawRefName, Site, TrackingRef, VcsErrorKind};

    /// A local branch name, obtained the only way one can be: through the
    /// named projection off a declared tracking ref.
    fn local(name: &str) -> LocalRefName {
        TrackingRef::parse(RawRefName::new(name))
            .expect("test fixture names are valid tracking refs")
            .local_counterpart()
    }

    fn on_remote(name: &str) -> RemoteRef {
        TrackingRef::parse(RawRefName::new(name))
            .expect("test fixture names are valid tracking refs")
            .on_remote()
    }

    /// Run git in `dir`, panicking on failure.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = git_command()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed in {}: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A repo on `main` with one commit.
    fn repo() -> tempfile::TempDir {
        let tmp = crate::tempdir().unwrap();
        let p = tmp.path();
        git(p, &["init", "-b", "main"]);
        git(p, &["config", "user.email", "t@t"]);
        git(p, &["config", "user.name", "T"]);
        std::fs::write(p.join("f"), "1").unwrap();
        git(p, &["add", "."]);
        git(p, &["commit", "-m", "one"]);
        tmp
    }

    /// A clone of `origin`, with the remote configured.
    ///
    /// Needed wherever a refusal is under test: git's `checkout.guess` only
    /// invents a branch when a *configured* remote maps a tracking ref to
    /// it, so a fixture that merely writes `refs/remotes/origin/x` by hand
    /// would let the birth pass unobserved.
    fn clone_of(origin: &Path) -> (tempfile::TempDir, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let dest = tmp.path().join("clone");
        git(
            tmp.path(),
            &["clone", origin.to_str().unwrap(), dest.to_str().unwrap()],
        );
        git(&dest, &["config", "user.email", "t@t"]);
        git(&dest, &["config", "user.name", "T"]);
        (tmp, dest)
    }

    /// Commit a new file and return the resulting tip.
    fn commit(p: &Path, name: &str) -> ResolvedRevisionId {
        std::fs::write(p.join(name), name).unwrap();
        git(p, &["add", "."]);
        git(p, &["commit", "-m", name]);
        GitVcs.head_revision(p).unwrap()
    }

    fn found(store: &Path, name: &str, at: &ResolvedRevisionId) -> FoundBranch {
        FoundBranch::named(store.to_path_buf(), name, at.clone())
    }

    /// A worktree destination that does not exist yet, inside its own temp
    /// directory so parallel tests cannot collide on it.
    fn worktree_dest() -> (tempfile::TempDir, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let dest = tmp.path().join("wt");
        (tmp, dest)
    }

    // -----------------------------------------------------------------------
    // Birth: authored vs adopted
    // -----------------------------------------------------------------------

    #[test]
    fn create_worktree_on_authors_the_ref_and_says_so() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        let (_dest_home, dest) = worktree_dest();

        let origin = GitVcs
            .create_worktree_on(&found(store.path(), "p--ww", &start), &dest)
            .unwrap();

        assert_eq!(origin, RefOrigin::Authored);
        // Which ref is this checkout on?
        assert_eq!(
            GitVcs.head_attachment(&dest).unwrap().to_string(),
            "on branch 'p--ww'"
        );
    }

    #[test]
    fn create_worktree_on_adopts_a_pre_existing_ref_without_destroying_it() {
        // The shipped retry force-deleted the branch and re-created it,
        // which destroyed a ref on nothing but a name match. A DESTROY
        // needs a proof; this path has none, so it must adopt.
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        git(store.path(), &["checkout", "p--ww"]);
        let unique = commit(store.path(), "unique-work");
        git(store.path(), &["checkout", "main"]);

        let (_dest_home, dest) = worktree_dest();
        let origin = GitVcs
            .create_worktree_on(&found(store.path(), "p--ww", &start), &dest)
            .unwrap();

        assert_eq!(origin, RefOrigin::Adopted);
        assert_eq!(
            GitVcs.head_attachment(&dest).unwrap().to_string(),
            "on branch 'p--ww'"
        );
        assert_eq!(
            GitVcs.head_revision(&dest).unwrap(),
            unique,
            "the commit that was already on the adopted branch survives"
        );
    }

    #[test]
    fn create_worktree_on_adopts_the_branch_when_a_tag_shares_its_name() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        git(store.path(), &["checkout", "p--ww"]);
        let unique = commit(store.path(), "unique-work");
        git(store.path(), &["checkout", "main"]);
        git(store.path(), &["tag", "p--ww", start.id().as_str()]);

        let (_dest_home, dest) = worktree_dest();
        GitVcs
            .create_worktree_on(&found(store.path(), "p--ww", &start), &dest)
            .unwrap();

        assert_eq!(
            GitVcs.head_attachment(&dest).unwrap().to_string(),
            "on branch 'p--ww'"
        );
        assert_eq!(GitVcs.head_revision(&dest).unwrap(), unique);
    }

    // -----------------------------------------------------------------------
    // Birth: a hook refusing the add is not a generic command failure
    // -----------------------------------------------------------------------

    /// Plant a `post-checkout` hook that refuses and prints nothing.
    ///
    /// Silence is the point: git contributes no mention of a hook to its own
    /// output, so a hook that also says nothing leaves the word absent from
    /// every byte an operator or a matcher could read.
    ///
    /// A caller must assert that the add FAILED before it asserts how the
    /// failure was classified. That assertion is what proves the hook fired.
    fn plant_silent_refusing_hook(repo: &Path) {
        let hooks = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("post-checkout");
        crate::write_executable(&hook, "#!/bin/sh\nexit 1\n");
    }

    #[test]
    fn a_refusing_hook_is_reported_as_a_hook_rejection() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        let (_dest_home, dest) = worktree_dest();
        plant_silent_refusing_hook(store.path());

        let err = GitVcs
            .create_worktree_on(&found(store.path(), "p--ww", &start), &dest)
            .expect_err("the hook refused, so the add failed");

        let VcsError::HookRejected { stderr, .. } = &err else {
            panic!("a refusing hook must classify as HookRejected, got: {err:?}");
        };
        assert!(
            !stderr.contains("hook"),
            "git's output must not contain the word, or this test cannot tell a \
             typed classification from a text match: {stderr:?}"
        );
    }

    /// A destination that is *already a registered worktree* is the case where
    /// registration-present is available without any hook having run: the
    /// registration is the previous call's, and this add fails on the taken
    /// path.
    #[test]
    fn an_add_onto_an_already_registered_worktree_is_not_a_hook_rejection() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        let (_dest_home, dest) = worktree_dest();

        GitVcs
            .create_worktree_on(&found(store.path(), "p--ww", &start), &dest)
            .expect("first add succeeds and registers dest");

        let err = GitVcs
            .create_worktree_on(&found(store.path(), "p--ww2", &start), &dest)
            .expect_err("the destination is taken, so the second add failed");

        assert!(
            matches!(err, VcsError::CommandFailed { .. }),
            "no hook ran; a registration this call did not create must not read \
             as a rejection, got: {err:?}"
        );
    }

    /// The classification cannot be reading text: this fixture puts "hook" in
    /// the destination path — which git echoes into the stderr of the failure
    /// — while no hook exists at all.
    #[test]
    fn an_unrelated_failure_under_a_hook_shaped_path_is_not_a_hook_rejection() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        let home = crate::tempdir().unwrap();
        let dest = home.path().join("hooks").join("wt");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("occupant"), "in the way").unwrap();

        let err = GitVcs
            .create_worktree_on(&found(store.path(), "p--ww", &start), &dest)
            .expect_err("the destination is occupied, so the add failed");

        assert!(
            err.to_string().contains("hook"),
            "fixture must put the word in the error text, or it proves nothing: {err}"
        );
        assert!(
            matches!(err, VcsError::CommandFailed { .. }),
            "an occupied destination is not a hook rejection however the path is \
             spelled, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // DESTROY: a proof
    // -----------------------------------------------------------------------

    fn main_name() -> RawRefName {
        RawRefName::new("main")
    }

    fn ww() -> RawRefName {
        RawRefName::new("p--ww")
    }

    fn landed_on(
        store: &Path,
        branch: &RawRefName,
        onto: &ResolvedRevisionId,
    ) -> Option<DestroyProof> {
        DestroyProof::landed(
            &GitVcs,
            store,
            Site::Local,
            branch,
            std::slice::from_ref(onto),
        )
        .unwrap()
    }

    #[test]
    fn a_branch_nothing_was_committed_on_is_landed_on_where_it_started() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);

        assert!(landed_on(store.path(), &ww(), &start).is_some());

        git(store.path(), &["checkout", "p--ww"]);
        commit(store.path(), "work");
        git(store.path(), &["checkout", "main"]);
        assert!(landed_on(store.path(), &ww(), &start).is_none());
    }

    #[test]
    fn landed_is_none_for_a_branch_that_does_not_exist() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        assert!(landed_on(store.path(), &RawRefName::new("never-created"), &start).is_none());
    }

    #[test]
    fn landed_holds_exactly_when_the_tip_is_reachable_from_a_name_that_outlives_it() {
        let store = repo();
        git(store.path(), &["branch", "p--ww"]);

        let advanced = commit(store.path(), "later");
        assert!(landed_on(store.path(), &ww(), &advanced).is_some());

        git(store.path(), &["checkout", "p--ww"]);
        commit(store.path(), "branch-only");
        git(store.path(), &["checkout", "main"]);
        assert!(landed_on(store.path(), &ww(), &advanced).is_none());
    }

    #[test]
    fn landed_on_any_of_several_names_is_enough() {
        let store = repo();
        let base = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["checkout", "-b", "p--ww"]);
        let work = commit(store.path(), "work");
        git(store.path(), &["checkout", "main"]);
        let elsewhere = commit(store.path(), "elsewhere");

        let proof = DestroyProof::landed(
            &GitVcs,
            store.path(),
            Site::Local,
            &ww(),
            &[base, elsewhere, work.clone()],
        )
        .unwrap()
        .expect("the branch's own tip is among the names");
        assert!(proof.describe().starts_with("landed on "));
    }

    #[test]
    fn delete_branch_destroys_the_branch_the_proof_names() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        git(store.path(), &["config", "branch.p--ww.remote", "origin"]);
        git(
            store.path(),
            &["config", "branch.p--ww.merge", "refs/heads/p--ww"],
        );

        let proof = landed_on(store.path(), &ww(), &start).expect("landed");
        GitVcs.delete_branch(proof).unwrap();

        assert!(GitVcs
            .resolve_local_branch_tip(store.path(), &ww())
            .unwrap()
            .is_none());
        assert!(
            GitVcs
                .resolve_local_branch_tip(store.path(), &main_name())
                .unwrap()
                .is_some(),
            "nothing else was touched"
        );
        let config = git_command()
            .args(["config", "--get-regexp", "^branch\\.p--ww"])
            .current_dir(store.path())
            .output()
            .unwrap();
        assert!(
            !config.status.success(),
            "the branch's config section goes with it: {}",
            String::from_utf8_lossy(&config.stdout)
        );
    }

    #[test]
    fn delete_branch_leaves_the_settings_of_a_branch_named_below_it() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        git(store.path(), &["config", "branch.p--ww.x.remote", "origin"]);

        let proof = landed_on(store.path(), &ww(), &start).expect("landed");
        GitVcs.delete_branch(proof).unwrap();

        let config = git_command()
            .args(["config", "--get", "branch.p--ww.x.remote"])
            .current_dir(store.path())
            .output()
            .unwrap();
        assert!(
            config.status.success(),
            "the settings of p--ww.x are not p--ww's"
        );
    }

    #[test]
    fn a_proof_keyed_to_a_git_dir_still_names_a_workable_repo() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);

        let proof = landed_on(&store.path().join(".git"), &ww(), &start).expect("landed");
        GitVcs.delete_branch(proof).unwrap();
        assert!(GitVcs
            .resolve_local_branch_tip(store.path(), &ww())
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_branch_that_moved_since_its_proof_is_not_deleted() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        let proof = landed_on(store.path(), &ww(), &start).expect("landed");

        git(store.path(), &["checkout", "p--ww"]);
        let work = commit(store.path(), "committed after the check");
        git(store.path(), &["checkout", "main"]);

        GitVcs
            .delete_branch(proof)
            .expect_err("the branch is no longer the one the proof is about");
        assert_eq!(
            GitVcs
                .resolve_local_branch_tip(store.path(), &ww())
                .unwrap(),
            Some(work)
        );
    }

    #[test]
    fn a_checked_out_branch_is_not_deleted() {
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        let proof = landed_on(store.path(), &main_name(), &start).expect("landed");

        let err = GitVcs
            .delete_branch(proof)
            .expect_err("main is checked out here");
        assert!(format!("{err}").contains("checked out"), "{err}");
        assert!(GitVcs
            .resolve_local_branch_tip(store.path(), &main_name())
            .unwrap()
            .is_some());
    }

    #[test]
    fn an_unlanded_branch_is_destroyed_only_on_the_operator_s_say_so_and_kept_as_a_backup() {
        let store = repo();
        let base = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        git(store.path(), &["checkout", "p--ww"]);
        let work = commit(store.path(), "unlanded-work");
        git(store.path(), &["checkout", "main"]);

        assert!(landed_on(store.path(), &ww(), &base).is_none());

        let proof = DestroyProof::backed_up(
            &GitVcs,
            store.path(),
            Site::Local,
            &ww(),
            DiscardUnlandedConsent::granted(),
        )
        .unwrap();
        let kept = proof.backup().expect("rests on a backup").backup().clone();
        GitVcs.delete_branch(proof).unwrap();
        assert!(GitVcs
            .resolve_local_branch_tip(store.path(), &ww())
            .unwrap()
            .is_none());

        assert_eq!(kept.tip, work);
        let listed = GitVcs
            .list_backups(store.path(), RemoteTracking::Counted)
            .unwrap();
        assert_eq!(
            listed.iter().map(|l| &l.backup).collect::<Vec<_>>(),
            vec![&kept],
            "the discarded tip stays reachable through the backup the proof rests on"
        );
        assert_eq!(
            listed[0]
                .held
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            vec![work.id().clone()]
        );
    }

    #[test]
    fn a_proof_says_what_licensed_the_destroy() {
        let store = repo();
        let base = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);

        let backed_up = DestroyProof::backed_up(
            &GitVcs,
            store.path(),
            Site::Local,
            &ww(),
            DiscardUnlandedConsent::granted(),
        )
        .unwrap();
        assert_eq!(
            backed_up.describe(),
            format!(
                "kept as backup {}",
                backed_up.backup().unwrap().backup().name
            )
        );
        assert!(landed_on(store.path(), &ww(), &base)
            .unwrap()
            .backup()
            .is_none());
    }

    fn published_and_landed() -> (tempfile::TempDir, PathBuf, PathBuf, DestroyProof) {
        let (tmp, clone, bare) = clone_of_bare_origin();
        git(&clone, &["checkout", "-b", "p--ww"]);
        commit(&clone, "published");
        git(&clone, &["push", "origin", "p--ww"]);
        git(&clone, &["checkout", "main"]);
        git(&clone, &["merge", "--ff-only", "p--ww"]);
        let main_tip = GitVcs.head_revision(&clone).unwrap();
        let proof =
            DestroyProof::landed(&GitVcs, &clone, Site::Remote(origin()), &ww(), &[main_tip])
                .unwrap()
                .expect("the remote copy is reached by main");
        (tmp, clone, bare, proof)
    }

    fn remote_has(bare: &Path, branch: &str) -> bool {
        git_command()
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(bare)
            .output()
            .unwrap()
            .status
            .success()
    }

    #[test]
    fn a_landed_remote_branch_is_deleted_and_its_tracking_ref_goes() {
        let (_tmp, clone, bare, proof) = published_and_landed();
        GitVcs.delete_branch(proof).unwrap();
        assert!(!remote_has(&bare, "p--ww"));
        assert!(GitVcs.remote_copies(&clone, &ww()).unwrap().is_empty());
    }

    #[test]
    fn a_remote_branch_that_moved_since_its_proof_is_not_deleted() {
        let (_tmp, clone, bare, proof) = published_and_landed();
        let other = crate::tempdir().unwrap();
        git(other.path(), &["clone", bare.to_str().unwrap(), "pusher"]);
        let pusher = other.path().join("pusher");
        git(&pusher, &["config", "user.email", "t@t"]);
        git(&pusher, &["config", "user.name", "T"]);
        git(&pusher, &["checkout", "p--ww"]);
        commit(&pusher, "pushed meanwhile");
        git(&pusher, &["push", "origin", "p--ww"]);

        GitVcs
            .delete_branch(proof)
            .expect_err("the remote moved since the proof was made");
        assert!(remote_has(&bare, "p--ww"));

        git(&clone, &["fetch", "origin"]);
        let main_tip = GitVcs.head_revision(&clone).unwrap();
        assert!(
            DestroyProof::landed(&GitVcs, &clone, Site::Remote(origin()), &ww(), &[main_tip])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_taken_destination_path_is_not_mistaken_for_a_taken_ref() {
        // git says "already exists" for both, and reading that string as
        // "the branch is already there" would send a filesystem collision
        // down the adopt path.
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        let (_dest_home, dest) = worktree_dest();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("in-the-way"), "x").unwrap();

        let err = GitVcs
            .create_worktree_on(&found(store.path(), "p--ww", &start), &dest)
            .unwrap_err();
        assert!(
            format!("{err}").contains("already exists"),
            "the real failure is surfaced, not swallowed: {err}"
        );
        assert!(
            dest.join("in-the-way").exists(),
            "the destination is intact"
        );
        // git writes the ref before it checks the destination, so a residual
        // `p--ww` at the start point is expected. What matters is that it is
        // at the start point and NOT destroyed on the way out.
        if let Some(tip) = GitVcs
            .resolve_local_branch_tip(store.path(), &RawRefName::new("p--ww"))
            .unwrap()
        {
            assert_eq!(tip, start, "the residual ref is exactly what was asked for");
        }
    }

    #[test]
    fn resolve_local_branch_tip_will_not_answer_with_a_tag() {
        let store = repo();
        let head = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["tag", "decoy"]);
        assert!(GitVcs
            .resolve_local_branch_tip(store.path(), &RawRefName::new("decoy"))
            .unwrap()
            .is_none());
        assert_eq!(
            GitVcs
                .resolve_local_branch_tip(store.path(), &RawRefName::new("main"))
                .unwrap(),
            Some(head)
        );
    }

    /// A clone whose local `main` has moved past `refs/remotes/origin/main`,
    /// and the remote-tracking tip.
    fn clone_ahead_of_its_remote() -> (tempfile::TempDir, PathBuf, ResolvedRevisionId) {
        let (tmp, clone, _bare) = clone_of_bare_origin();
        let remote_tip = GitVcs
            .resolve_revision(&clone, "refs/remotes/origin/main")
            .unwrap();
        let local_tip = commit(&clone, "local");
        assert_ne!(local_tip, remote_tip, "the shadow must point elsewhere");
        (tmp, clone, remote_tip)
    }

    #[test]
    fn resolve_branch_on_remote_is_not_answered_by_a_local_branch_named_origin_slash_main() {
        let (_tmp, clone, remote_tip) = clone_ahead_of_its_remote();
        git(&clone, &["branch", "origin/main"]);

        let resolved = GitVcs
            .resolve_branch_on_remote(&clone, &origin(), &on_remote("main"))
            .unwrap();

        assert_eq!(resolved, remote_tip);
    }

    #[test]
    fn resolve_branch_on_remote_is_not_answered_by_a_tag_named_origin_slash_main() {
        let (_tmp, clone, remote_tip) = clone_ahead_of_its_remote();
        git(&clone, &["tag", "origin/main"]);

        let resolved = GitVcs
            .resolve_branch_on_remote(&clone, &origin(), &on_remote("main"))
            .unwrap();

        assert_eq!(resolved, remote_tip);
    }

    #[test]
    fn resolve_branch_on_remote_finds_nothing_when_only_a_shadow_exists() {
        let (_tmp, clone, _remote_tip) = clone_ahead_of_its_remote();
        git(&clone, &["branch", "origin/feature"]);

        let err = GitVcs
            .resolve_branch_on_remote(&clone, &origin(), &on_remote("feature"))
            .expect_err("a local branch named origin/feature is not a remote-tracking ref");

        assert!(
            matches!(err, VcsError::RevisionNotFound { ref rev, .. } if rev == "refs/remotes/origin/feature"),
            "got {err:?}"
        );
    }

    #[test]
    fn count_commits_ahead_of_remote_is_not_answered_by_a_tag_named_like_the_branch() {
        let (_tmp, clone, _remote_tip) = clone_ahead_of_its_remote();
        git(&clone, &["tag", "main", "refs/remotes/origin/main"]);

        let ahead = GitVcs
            .count_commits_ahead_of_remote(&clone, &RawRefName::new("main"))
            .unwrap();

        assert_eq!(
            ahead, 1,
            "the unpushed commit is on the branch, not the tag"
        );
    }

    // -----------------------------------------------------------------------
    // ATTACH: post-birth attachment changes need consent
    // -----------------------------------------------------------------------

    #[test]
    fn detach_head_leaves_the_checkout_on_no_branch() {
        let p = repo();
        let target = GitVcs.head_revision(p.path()).unwrap();
        let HeadAttachment::Attached(w) = GitVcs.head_attachment(p.path()).unwrap() else {
            panic!("fixture should be attached");
        };

        GitVcs
            .detach_head(&w, &target, &crate::alignment::Departure::granted())
            .unwrap();

        match GitVcs.head_attachment(p.path()).unwrap() {
            HeadAttachment::Detached(d) => assert_eq!(d.at(), &target),
            other => panic!("expected Detached, got {other:?}"),
        }
    }

    #[test]
    fn detach_head_refuses_a_stale_witness() {
        let p = repo();
        let target = GitVcs.head_revision(p.path()).unwrap();
        let HeadAttachment::Attached(w) = GitVcs.head_attachment(p.path()).unwrap() else {
            panic!("fixture should be attached");
        };
        git(p.path(), &["checkout", "-b", "elsewhere"]);

        let err = GitVcs
            .detach_head(&w, &target, &crate::alignment::Departure::granted())
            .unwrap_err();
        assert_eq!(err.kind(), VcsErrorKind::StaleRefWitness);
        assert_eq!(
            GitVcs.head_attachment(p.path()).unwrap().to_string(),
            "on branch 'elsewhere'",
            "the refusal left the attachment alone"
        );
    }

    #[test]
    fn reattach_head_moves_the_checkout_onto_an_existing_branch() {
        let p = repo();
        let tip = GitVcs.head_revision(p.path()).unwrap();
        git(p.path(), &["branch", "target"]);
        git(p.path(), &["checkout", "--detach", tip.id().as_str()]);

        let from = GitVcs.head_attachment(p.path()).unwrap();
        GitVcs
            .reattach_head(
                from,
                &local("target"),
                &crate::alignment::Departure::granted(),
            )
            .unwrap();

        assert_eq!(
            GitVcs.head_attachment(p.path()).unwrap().to_string(),
            "on branch 'target'"
        );
    }

    #[test]
    fn reattach_head_refuses_when_the_planned_state_no_longer_holds() {
        let p = repo();
        let tip = GitVcs.head_revision(p.path()).unwrap();
        git(p.path(), &["branch", "target"]);
        git(p.path(), &["checkout", "--detach", tip.id().as_str()]);

        let from = GitVcs.head_attachment(p.path()).unwrap();
        // The operator reattaches by hand first.
        git(p.path(), &["checkout", "main"]);

        let err = GitVcs
            .reattach_head(
                from,
                &local("target"),
                &crate::alignment::Departure::granted(),
            )
            .unwrap_err();
        assert_eq!(err.kind(), VcsErrorKind::StaleRefWitness);
        assert_eq!(
            GitVcs.head_attachment(p.path()).unwrap().to_string(),
            "on branch 'main'"
        );
    }

    #[test]
    fn reattach_head_will_not_create_the_branch_it_attaches_to() {
        // Attaching to a branch that does not exist would be a birth, which
        // has a different consent shape. A bare repo with no remote, no tag
        // and no colliding path proves nothing here — those are exactly the
        // conditions under which git does something other than refuse — so
        // the fixture has all three, and each name is one a manifest
        // `branch` could plausibly declare.
        let origin = repo();
        git(origin.path(), &["branch", "feature"]);
        let (_home, work) = clone_of(origin.path());
        git(&work, &["tag", "tagname"]);
        std::fs::create_dir(work.join("docs")).unwrap();
        std::fs::write(work.join("docs/a.md"), "committed").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "docs"]);

        let tip = GitVcs.head_revision(&work).unwrap();
        git(&work, &["checkout", "--detach", tip.id().as_str()]);
        // An uncommitted edit to the colliding path. Pathspec mode reverts
        // it, which is the data loss, so it is asserted after every case.
        std::fs::write(work.join("docs/a.md"), "operator edit").unwrap();

        for name in [
            "feature", // refs/remotes/origin/feature — checkout.guess births
            "tagname", // refs/tags/tagname — checkout detaches
            "docs",    // a tracked path — checkout takes it as a pathspec
            "nothing", // absent outright
        ] {
            let from = GitVcs.head_attachment(&work).unwrap();
            let err = GitVcs
                .reattach_head(from, &local(name), &crate::alignment::Departure::granted())
                .unwrap_err();

            assert_eq!(
                err.kind(),
                VcsErrorKind::CommandFailed,
                "{name}: expected a refusal"
            );
            assert!(
                GitVcs
                    .resolve_local_branch_tip(&work, &RawRefName::new(name))
                    .unwrap()
                    .is_none(),
                "{name}: the refusal must not have created the branch"
            );
            assert!(
                matches!(
                    GitVcs.head_attachment(&work).unwrap(),
                    HeadAttachment::Detached(_)
                ),
                "{name}: the refusal must leave HEAD where it was"
            );
            assert_eq!(
                std::fs::read_to_string(work.join("docs/a.md")).unwrap(),
                "operator edit",
                "{name}: the refusal must not touch the working tree"
            );
        }
    }

    #[test]
    fn the_attach_refusal_reports_a_command_that_reproduces_its_answer() {
        // The refusal explains itself by naming the query it rests on, which
        // is only worth anything if running that query verbatim gives the
        // same answer. `--quiet` is the load-bearing part: without it the
        // same rev-parse exits 128 "Needed a single revision" instead of the
        // silent exit 1 the absence branch reads as "no such ref", so a
        // reported form that dropped it would send the operator to a
        // different failure than the one being explained.
        let (_home, work) = clone_of(repo().path());

        let from = GitVcs.head_attachment(&work).unwrap();
        let err = GitVcs
            .reattach_head(
                from,
                &local("absent"),
                &crate::alignment::Departure::granted(),
            )
            .unwrap_err();

        let VcsError::CommandFailed { args, repo, .. } = err else {
            panic!("expected the refusal to report the query that refused");
        };

        assert_eq!(
            args,
            vec![
                "rev-parse".to_owned(),
                "--verify".to_owned(),
                "--quiet".to_owned(),
                "refs/heads/absent^{commit}".to_owned(),
            ],
            "the reported argv must be the one resolve_local_branch_tip ran"
        );
        assert_eq!(
            repo,
            GitVcs::work_dir_for_store(&work),
            "the reported directory must be the one the query ran in"
        );

        // Run exactly what was reported and confirm it reproduces "absent":
        // no stdout, and a failure that is NOT the 128 the un-quiet form gives.
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        assert!(
            matches!(
                GitVcs::run(&borrowed, &repo),
                Err(VcsError::CommandFailed { .. })
            ),
            "the reported command must reproduce the absence the refusal rests on"
        );
        let out = std::process::Command::new("git")
            .args(&borrowed)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "--quiet makes absence exit 1");
        assert!(
            out.stderr.is_empty(),
            "--quiet makes absence silent; got {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn reattach_head_still_attaches_when_a_tag_shares_the_branch_name() {
        // The refusals above must not be bought by refusing everything: an
        // existing branch is still attachable when a tag, a remote-tracking
        // ref and a path all carry the same name.
        let origin = repo();
        git(origin.path(), &["branch", "shared"]);
        let (_home, work) = clone_of(origin.path());
        git(&work, &["branch", "shared", "origin/shared"]);
        git(&work, &["tag", "shared"]);
        std::fs::create_dir(work.join("shared")).unwrap();
        std::fs::write(work.join("shared/f"), "x").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "collide"]);

        let tip = GitVcs.head_revision(&work).unwrap();
        git(&work, &["checkout", "--detach", tip.id().as_str()]);
        let from = GitVcs.head_attachment(&work).unwrap();

        GitVcs
            .reattach_head(
                from,
                &local("shared"),
                &crate::alignment::Departure::granted(),
            )
            .unwrap();

        assert_eq!(
            GitVcs.head_attachment(&work).unwrap().to_string(),
            "on branch 'shared'",
            "the branch wins over the tag, the remote ref and the path"
        );
    }

    // -----------------------------------------------------------------------
    // Savepoint keys: the ref names an op's savepoints have always had
    // -----------------------------------------------------------------------

    fn both_sides(op: &str) -> [(SavepointKey, String); 2] {
        let op: OpId = op.parse().unwrap();
        [
            (SavepointKey::Op(op.clone()), op.to_string()),
            (SavepointKey::LandTarget(op.clone()), format!("{op}-target")),
        ]
    }

    #[test]
    fn savepoint_refs_keep_their_names_on_both_sides() {
        let p = repo();
        let head = GitVcs.head_revision(p.path()).unwrap();
        for (key, leaf) in both_sides("1779769917405921588") {
            GitVcs.create_savepoint(p.path(), &key).unwrap();

            assert_eq!(
                git(
                    p.path(),
                    &[
                        "rev-parse",
                        &format!("refs/rwv/pre-op/{leaf}/checkout/main")
                    ]
                ),
                head.id().as_str()
            );
            assert_eq!(
                GitVcs.savepoint_label(&key),
                format!("refs/rwv/pre-op/{leaf}")
            );
        }
    }

    #[test]
    fn the_two_sides_of_an_op_do_not_share_a_savepoint() {
        let p = repo();
        let [(owner, _), (target, _)] = both_sides("1779769917405921588");
        GitVcs.create_savepoint(p.path(), &owner).unwrap();

        assert!(GitVcs.resolve_savepoint(p.path(), &owner).is_some());
        assert_eq!(GitVcs.resolve_savepoint(p.path(), &target), None);

        GitVcs.drop_savepoint(p.path(), &owner);
        assert_eq!(GitVcs.resolve_savepoint(p.path(), &owner), None);
    }

    #[test]
    fn savepoints_listed_read_back_as_the_keys_that_filed_them() {
        let p = repo();
        let [(owner, _), (target, _)] = both_sides("1779769917405921588");
        GitVcs.create_savepoint(p.path(), &owner).unwrap();
        GitVcs.create_savepoint(p.path(), &target).unwrap();
        git(
            p.path(),
            &["update-ref", "refs/rwv/pre-op/not.a.key", "HEAD"],
        );

        let listing = GitVcs.list_savepoints(p.path()).unwrap();
        let mut keys: Vec<String> = listing.keys.iter().map(ToString::to_string).collect();
        keys.sort();

        assert_eq!(
            keys,
            vec![
                "1779769917405921588".to_owned(),
                "1779769917405921588-target".to_owned()
            ],
        );
        assert_eq!(
            listing.unrecognized,
            vec!["refs/rwv/pre-op/not.a.key".to_owned()],
            "a ref no key names is not a savepoint rwv filed, and is listed apart"
        );
    }

    // -----------------------------------------------------------------------
    // Unborn HEAD: a state, and not one a MOVE can reach
    // -----------------------------------------------------------------------

    #[test]
    fn an_unborn_head_yields_no_witness_to_move() {
        let tmp = crate::tempdir().unwrap();
        git(tmp.path(), &["init", "-b", "main"]);

        let observed = GitVcs.head_attachment(tmp.path()).unwrap();
        assert!(
            matches!(observed, HeadAttachment::Unborn(_)),
            "expected Unborn, got {observed:?}"
        );
        assert!(
            observed.attached().is_none(),
            "an UnbornRef is not an AttachedRef, so there is nothing to pass \
             to advance_attached_ref — MOVE semantics on an unborn HEAD are \
             undefined, so the call is unrepresentable rather than wrong"
        );
        assert_eq!(
            observed.to_string(),
            "on unborn branch 'main' (no commits yet)"
        );
    }

    // -----------------------------------------------------------------------
    // Publish: the ref is a parameter
    // -----------------------------------------------------------------------
    //
    // These moved here from `tests/vcs/vcs_test.rs`, where they exercised
    // `push_with_role` — deleted with the rest of the old surface. They could
    // not follow `push_ref` out of the crate: a `PublishRef` is minted only
    // by `PublishRef::from_attached`, which is `pub(crate)`, so an
    // integration test cannot construct one. That is the seal working, not an
    // obstacle to route around by widening the constructor.
    //
    // One case did NOT move: `push_with_role_detached_head_errors`. Its
    // guarantee left the impl when the ref became a parameter — there is no
    // longer a `current_ref` read inside `push_ref` to fail on a detached
    // HEAD, because a detached checkout yields no `AttachedRef` and therefore
    // no `PublishRef`. The refusal is now the publish gate's, and it is
    // pinned end-to-end at `tests/sync/push_test.rs`
    // (`push_refuses_detached_head`, and `NotARepo` at :528).

    /// Read a ref out of a BARE repo.
    ///
    /// `--git-dir` rather than `current_dir`, because `git_command` does not
    /// clear `GIT_CONFIG_*` from the environment: under an ambient
    /// `safe.bareRepository=explicit` (which this repo's own tooling sets),
    /// git refuses to discover a bare repo from the cwd and only accepts one
    /// named outright. That is what "explicit" means, so name it.
    fn git_bare(bare: &Path, args: &[&str]) -> String {
        let mut all = vec!["--git-dir".to_owned(), bare.to_string_lossy().into_owned()];
        all.extend(args.iter().map(|a| (*a).to_owned()));
        let out = git_command().args(&all).output().expect("run git");
        assert!(
            out.status.success(),
            "git {all:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A bare origin with `main` at one commit, plus a clone of it.
    ///
    /// Bare deliberately: pushing into a non-bare checkout's current branch
    /// is refused by git itself, which would make a push test pass or fail
    /// for a reason that has nothing to do with the ref being published.
    /// The remote `git clone` names a clone's source: git's own default, which
    /// is what the tests of git's ref handling clone from.
    fn origin() -> RemoteHandle {
        RemoteHandle::new("origin")
    }

    fn clone_of_bare_origin() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let bare = tmp.path().join("origin.git");
        let seed = tmp.path().join("seed");
        let clone = tmp.path().join("clone");

        git(tmp.path(), &["init", "--bare", "-b", "main", "origin.git"]);
        git(
            tmp.path(),
            &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
        );
        git(&seed, &["config", "user.email", "t@t"]);
        git(&seed, &["config", "user.name", "T"]);
        std::fs::write(seed.join("f"), "1").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-m", "one"]);
        git(&seed, &["push", "origin", "main"]);

        git(
            tmp.path(),
            &["clone", bare.to_str().unwrap(), clone.to_str().unwrap()],
        );
        git(&clone, &["config", "user.email", "t@t"]);
        git(&clone, &["config", "user.name", "T"]);
        (tmp, clone, bare)
    }

    /// The `PublishRef` for whatever branch `repo` is attached to.
    fn publish_ref_of(repo: &Path) -> PublishRef {
        let HeadAttachment::Attached(a) = GitVcs.head_attachment(repo).unwrap() else {
            panic!("fixture should be attached");
        };
        let name = RawRefName::new(a.to_string());
        let tip = GitVcs
            .resolve_local_branch_tip(repo, &name)
            .unwrap()
            .unwrap();
        PublishRef::judged(&LocalRefName::of_branch(&name), tip)
    }

    #[test]
    fn push_ref_replaces_a_diverged_push_branch_only_while_it_holds_the_leased_tip() {
        let (tmp, clone, bare) = clone_of_bare_origin();
        let seeded = git_bare(&bare, &["rev-parse", "main"]);

        let other = tmp.path().join("other");
        git(
            tmp.path(),
            &["clone", bare.to_str().unwrap(), other.to_str().unwrap()],
        );
        git(&other, &["config", "user.email", "t@t"]);
        git(&other, &["config", "user.name", "T"]);
        let foreign = commit(&other, "foreign");
        git(&other, &["push", "origin", "main"]);

        let local_tip = commit(&clone, "local");
        let repo = RepoOperand::Member(RepoPath::new("m").unwrap());
        let consent = OverwriteConsent::granted(repo.clone());
        let leased_to = |tip: &str| {
            consent
                .lease(
                    &repo,
                    ResolvedRevisionId::from(CommitId::parse(tip).unwrap()),
                )
                .unwrap()
        };

        let err = GitVcs
            .push_ref(
                &clone,
                &origin(),
                &publish_ref_of(&clone),
                &Replace::FastForwardOnly,
            )
            .expect_err("a non-fast-forward push must refuse without a lease");
        assert_eq!(err.kind(), VcsErrorKind::CommandFailed);

        let err = GitVcs
            .push_ref(
                &clone,
                &origin(),
                &publish_ref_of(&clone),
                &Replace::Leased(leased_to(&seeded)),
            )
            .expect_err("a lease on a tip the remote has moved past must refuse");
        assert_eq!(err.kind(), VcsErrorKind::CommandFailed);
        assert_eq!(
            git_bare(&bare, &["rev-parse", "main"]),
            foreign.id().as_str(),
            "the refusal is a refusal: origin still holds the foreign tip"
        );

        GitVcs
            .push_ref(
                &clone,
                &origin(),
                &publish_ref_of(&clone),
                &Replace::Leased(leased_to(foreign.id().as_str())),
            )
            .expect("a lease on the tip the remote holds should replace it");
        assert_eq!(
            git_bare(&bare, &["rev-parse", "main"]),
            local_tip.id().as_str()
        );
    }

    #[test]
    fn push_ref_publishes_the_branch_when_a_tag_shares_its_name() {
        let (_tmp, clone, bare) = clone_of_bare_origin();
        let tip = commit(&clone, "on-main");
        git(&clone, &["tag", "main"]);

        GitVcs
            .push_ref(
                &clone,
                &origin(),
                &publish_ref_of(&clone),
                &Replace::FastForwardOnly,
            )
            .expect("a tag named like the branch must not make the push ambiguous");

        assert_eq!(
            git_bare(&bare, &["rev-parse", "refs/heads/main"]),
            tip.id().as_str()
        );
    }

    #[test]
    fn push_ref_publishes_the_ref_it_was_given_not_the_one_head_is_on() {
        // The ref is a parameter, so the choice is made at one site in
        // push.rs rather than being whatever branch the checkout happened to
        // be on when the impl ran.
        // Switching HEAD after the PublishRef is built must not redirect the
        // push — if the impl still read HEAD, this would publish `sideshow`.
        let (_tmp, clone, bare) = clone_of_bare_origin();
        let main_tip = commit(&clone, "on-main");
        let publish = publish_ref_of(&clone);

        git(&clone, &["checkout", "-b", "sideshow"]);
        let side_tip = commit(&clone, "on-sideshow");
        assert_ne!(main_tip, side_tip, "the two branches must differ");

        GitVcs
            .push_ref(&clone, &origin(), &publish, &Replace::FastForwardOnly)
            .unwrap();

        assert_eq!(
            git_bare(&bare, &["rev-parse", "main"]),
            main_tip.id().as_str(),
            "push_ref must publish the ref it was handed"
        );
        assert!(
            !git_bare(&bare, &["branch", "--list", "sideshow"]).contains("sideshow"),
            "the branch HEAD moved to must not have been published"
        );
    }

    #[test]
    fn work_dir_for_store_normalizes_a_git_dir_and_leaves_anything_else_alone() {
        let clone = Path::new("/w/github/acme/server");
        assert_eq!(GitVcs::work_dir_for_store(&clone.join(".git")), clone);
        assert_eq!(GitVcs::work_dir_for_store(clone), clone);
    }
}

#[cfg(test)]
mod derived_content_tests {
    //! Git's translation of a stated derived-content policy.
    //!
    //! In-crate because the resolution vocabulary is crate-internal: the
    //! translation is a match on it, and only code inside the crate can name
    //! the arms it has to cover. The behavior the translated arguments buy —
    //! a declared path resolving mechanically through a real replay — is in
    //! `tests/sync/derived_content_policy_test.rs`.

    use super::*;

    #[test]
    fn keeping_the_target_side_defines_both_drivers_inline_as_a_side_pick() {
        let args = derived_content_git_args(DerivedContentPolicy::keep_target_side());
        let pairs: Vec<&[String]> = args.chunks(2).collect();
        assert!(
            pairs.iter().all(|pair| pair.len() == 2 && pair[0] == "-c"),
            "each definition is a `-c KEY=VALUE` pair; got {args:?}"
        );
        let mut defined: Vec<&str> = pairs.iter().map(|pair| pair[1].as_str()).collect();
        defined.sort_unstable();
        assert_eq!(
            defined,
            [
                format!("{RWV_MERGE_DRIVER_CONFIG_KEY}=true"),
                format!("{DERIVED_CONTENT_DRIVER_CONFIG_KEY}=true"),
            ],
            "`true` exits 0 without touching the file, which is what leaves the target-side \
             version standing; anything else would run a command mid-replay"
        );
    }

    #[test]
    fn a_z_stream_is_read_as_terminated_records_not_separated_ones() {
        // `derived_content_dropped_by_replay` reads three `-z` streams and
        // reports what survives the last one. A parser that mistook the
        // trailing terminator for an empty record, or that dropped the final
        // record for lack of one, would report nothing — and reporting
        // nothing is indistinguishable from a replay that discarded nothing.
        assert_eq!(
            nul_terminated_fields("docs/a.md\0docs/b.md\0"),
            vec!["docs/a.md", "docs/b.md"]
        );
        assert_eq!(nul_terminated_fields(""), Vec::<String>::new());

        // What `check-attr -z` emits, whose triples the caller chunks.
        assert_eq!(
            nul_terminated_fields("docs/a.md\0merge\0rwv-ours\0").len() % 3,
            0
        );
    }

    #[test]
    fn the_vcs_default_supplies_nothing() {
        let args = derived_content_git_args(DerivedContentPolicy::vcs_default());
        assert!(
            args.is_empty(),
            "no resolver may be defined by an invocation that was told not to; got {args:?}"
        );
    }

    #[test]
    fn the_translation_is_the_only_thing_that_differs_between_the_policies() {
        // Both policies produce arguments that are *prefixes* — they say what
        // is in force, never what the operation is. A resolution that
        // smuggled a rebase flag in would silently change the replay.
        for policy in [
            DerivedContentPolicy::keep_target_side(),
            DerivedContentPolicy::vcs_default(),
        ] {
            for arg in derived_content_git_args(policy) {
                assert!(
                    arg == "-c" || arg.contains('='),
                    "policy {policy:?} contributed a non-configuration argument: {arg:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod merge_driver_vocabulary_tests {
    //! `check::phantom_merge_drivers_in` filters `.gitattributes` driver
    //! names through [`RWV_MERGE_DRIVER_PREFIX`] before checking them
    //! against the names rwv defines. Nothing but this test ties the two
    //! together — a rename of one that drops the other out of the prefix
    //! would compile cleanly and doctor's phantom-driver scan would silently
    //! stop matching a live driver name.

    use super::*;

    #[test]
    fn the_driver_names_fall_under_their_own_scan_prefix() {
        assert!(RWV_MERGE_DRIVER_NAME.starts_with(RWV_MERGE_DRIVER_PREFIX));
        assert!(DERIVED_CONTENT_DRIVER_NAME.starts_with(RWV_MERGE_DRIVER_PREFIX));
    }

    #[test]
    fn a_declaration_rewrite_drops_every_superseded_lock_line_and_keeps_the_rest() {
        let text =
            "docs/** merge=rwv-ours\nrwv.lock merge=ours\r\n* text=auto\nrwv.lock merge=rwv-ours";
        assert_eq!(lock_attribute_in(Some(text)), LockDriverPart::Superseded);
        let rewritten = declare_lock_driver_in(Some(text)).expect("a superseded line is rewritten");
        assert_eq!(
            rewritten,
            "docs/** merge=rwv-ours\nrwv.lock merge=rwv-lock\r\n* text=auto\n"
        );
        assert_eq!(lock_attribute_in(Some(&rewritten)), LockDriverPart::Current);
        assert_eq!(declare_lock_driver_in(Some(&rewritten)), None);
        assert_eq!(
            declare_lock_driver_in(None).as_deref(),
            Some("rwv.lock merge=rwv-lock\n")
        );
        assert_eq!(lock_attribute_in(None), LockDriverPart::Absent);
    }
}

#[cfg(test)]
mod population_tests {
    //! Each answer is checked against git's own view of the same files: what
    //! `ls-tree` lists, what `cat-file` holds, what `checkout-index` writes.

    use super::*;
    use crate::vcs::FoundBranch;
    use std::collections::BTreeMap;

    static GIT: GitVcs = GitVcs;

    fn population() -> &'static dyn WorkspacePopulation {
        GIT.population()
            .expect("the git under test answers population")
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        String::from_utf8(git_bytes(dir, args))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
        let out = git_command()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed in {}: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// A repo on `main` whose first commit holds `files` and whose second
    /// adds `attributes` as `.gitattributes`, so no blob was normalized on
    /// the way in, and which checks out under `endings`.
    ///
    /// Workspaces made from it are its siblings, inside the same temp dir.
    fn repo_with(
        files: &[(&str, &[u8])],
        attributes: &str,
        endings: LineEndings,
    ) -> (tempfile::TempDir, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let p = repo.as_path();
        git(tmp.path(), &["init", "-q", "-b", "main", "repo"]);
        git(p, &["config", "user.email", "t@t"]);
        git(p, &["config", "user.name", "T"]);
        git(p, &["config", "core.autocrlf", "false"]);
        for (name, bytes) in files {
            std::fs::write(p.join(name), bytes).unwrap();
        }
        git(p, &["add", "."]);
        git(p, &["commit", "-q", "-m", "content"]);
        if !attributes.is_empty() {
            std::fs::write(p.join(".gitattributes"), attributes).unwrap();
            git(p, &["add", ".gitattributes"]);
            git(p, &["commit", "-q", "-m", "attributes"]);
        }
        endings.pin(p);
        (tmp, repo)
    }

    /// What git's own checkout writes at `path` in `workspace`, conversions
    /// included.
    fn checked_out_bytes(workspace: &Path, path: &str) -> Vec<u8> {
        git_bytes(
            workspace,
            &["cat-file", "--filters", &format!("HEAD:{path}")],
        )
    }

    fn head(repo: &Path) -> ResolvedRevisionId {
        ResolvedRevisionId::from_canonical(
            crate::vcs::CommitId::parse(&git(repo, &["rev-parse", "HEAD"])).unwrap(),
            None,
        )
    }

    fn found(repo: &Path, name: &str) -> FoundBranch {
        FoundBranch::named(repo.join(".git"), name, head(repo))
    }

    /// A workspace of `repo` with nothing written, on a branch of its own.
    fn unpopulated(repo: &Path, name: &str) -> PathBuf {
        let dest = repo.with_file_name(name);
        population()
            .create_unpopulated_worktree_on(&found(repo, name), &dest)
            .unwrap();
        dest
    }

    /// A workspace of `repo` git checked out itself.
    fn checked_out(repo: &Path, name: &str) -> PathBuf {
        let dest = repo.with_file_name(name);
        GIT.create_worktree_on(&found(repo, name), &dest).unwrap();
        dest
    }

    fn blob_bytes(repo: &Path, path: &str) -> Vec<u8> {
        git_bytes(repo, &["cat-file", "blob", &format!("HEAD:{path}")])
    }

    fn by_path(files: Vec<TrackedFile>) -> BTreeMap<String, TrackedFile> {
        files.into_iter().map(|f| (f.path.clone(), f)).collect()
    }

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    /// Commit a symlink and a nested repository by index entry alone, which
    /// works where the filesystem can make neither.
    fn commit_link_and_nested(repo: &Path) {
        let target =
            GitVcs::run_with_stdin(["hash-object", "-w", "--stdin"], repo, b"a.txt").unwrap();
        let target = target.trim();
        git(
            repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{target},link"),
            ],
        );
        let nested = git(repo, &["rev-parse", "HEAD"]);
        git(
            repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{nested},nested"),
            ],
        );
        git(repo, &["commit", "-q", "-m", "link and nested"]);
    }

    #[test]
    fn this_git_answers_population() {
        assert!(GIT.population().is_some());
    }

    #[test]
    fn git_declares_updates_replace_files_only_where_conformance_holds() {
        assert_eq!(
            population().updates_replace_files(),
            cfg!(target_os = "linux"),
            "on {}",
            std::env::consts::OS
        );
    }

    #[test]
    fn an_unpopulated_worktree_records_its_revision_and_writes_no_file() {
        let (_tmp, repo) = repo_with(
            &[("a.txt", b"a\n"), ("b.txt", b"b\n")],
            "",
            LineEndings::Verbatim,
        );
        let repo = repo.as_path();
        let dest = repo.with_file_name("w");

        let born = population()
            .create_unpopulated_worktree_on(&found(repo, "p--w"), &dest)
            .unwrap();

        assert_eq!(born, RefOrigin::Authored);
        let on_disk: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(on_disk, vec![std::ffi::OsString::from(".git")]);
        assert_eq!(git(&dest, &["symbolic-ref", "HEAD"]), "refs/heads/p--w");
        assert_eq!(git(&dest, &["rev-parse", "HEAD"]), head(repo).id().as_str());
        assert_eq!(
            git(&dest, &["ls-files", "--stage"]),
            git(repo, &["ls-files", "--stage"]),
            "the new workspace does not record its revision"
        );
    }

    #[test]
    fn an_unpopulated_worktree_adopts_a_branch_that_already_exists() {
        let (_tmp, repo) = repo_with(&[("a.txt", b"a\n")], "", LineEndings::Verbatim);
        let repo = repo.as_path();
        let before = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["branch", "p--w", "HEAD"]);
        std::fs::write(repo.join("a.txt"), "moved\n").unwrap();
        git(repo, &["commit", "-q", "-am", "moved"]);

        let born = population()
            .create_unpopulated_worktree_on(&found(repo, "p--w"), &repo.with_file_name("w"))
            .unwrap();

        assert_eq!(born, RefOrigin::Adopted);
        assert_eq!(git(repo, &["rev-parse", "p--w"]), before);
    }

    fn tracked_files_are_the_tree_with_its_kinds_and_identities_only_on_files(
        endings: LineEndings,
    ) {
        let (_tmp, repo) = repo_with(
            &[
                ("a.txt", b"a\n"),
                ("run.sh", b"#!/bin/sh\n"),
                ("data.bin", b"\x00\x01\n"),
            ],
            "*.bin -text\n",
            endings,
        );
        let repo = repo.as_path();
        git(repo, &["update-index", "--chmod=+x", "run.sh"]);
        git(repo, &["commit", "-q", "-m", "executable"]);
        commit_link_and_nested(repo);
        let dest = unpopulated(repo, "w");

        let tracked = by_path(population().tracked_files(&dest).unwrap());

        let tree = git(repo, &["ls-tree", "-r", "HEAD"]);
        assert_eq!(tracked.len(), tree.lines().count());
        for line in tree.lines() {
            let (meta, path) = line.split_once('\t').unwrap();
            let mode = meta.split(' ').next().unwrap();
            let kind = &tracked[path].kind;
            match mode {
                "100644" | "100755" => {
                    let TrackedFileKind::File {
                        executable,
                        identity,
                    } = kind
                    else {
                        panic!("{path} ({mode}) reported as {kind:?}");
                    };
                    assert_eq!(*executable, mode == "100755", "{path}");
                    let converted = endings == LineEndings::Converted && path != "data.bin";
                    assert_eq!(
                        identity.is_some(),
                        !converted,
                        "{path}: a file has an identity exactly where its checkout is \
                         not converted"
                    );
                }
                "120000" => assert_eq!(*kind, TrackedFileKind::Symlink, "{path}"),
                "160000" => assert_eq!(*kind, TrackedFileKind::NestedRepository, "{path}"),
                other => panic!("unexpected mode {other} for {path}"),
            }
        }
    }

    #[test]
    fn an_identity_names_the_conversion_context_not_just_the_blob() {
        let same = b"one\ntwo\n".as_slice();
        let (_tmp, repo) = repo_with(
            &[
                ("plain.txt", same),
                ("plain-too.txt", same),
                ("auto.txt", same),
                ("crlf.txt", same),
                ("filtered.txt", same),
            ],
            "auto.txt text=auto\ncrlf.txt eol=crlf\nfiltered.txt filter=upper\n",
            LineEndings::Verbatim,
        );
        let repo = repo.as_path();
        let dest = unpopulated(repo, "w");
        let identity =
            |files: &BTreeMap<String, TrackedFile>, path: &str| files[path].identity().cloned();

        let verbatim = by_path(population().tracked_files(&dest).unwrap());
        git(repo, &["config", "core.autocrlf", "input"]);
        let input = by_path(population().tracked_files(&dest).unwrap());
        git(repo, &["config", "core.autocrlf", "true"]);
        let crlf_everywhere = by_path(population().tracked_files(&dest).unwrap());

        assert!(identity(&verbatim, "plain.txt").is_some());
        assert_eq!(
            identity(&verbatim, "plain.txt"),
            identity(&verbatim, "plain-too.txt"),
            "one blob in one context is one identity"
        );
        assert_ne!(
            identity(&verbatim, "plain.txt"),
            identity(&verbatim, "auto.txt"),
            "an attribute is not in the identity"
        );
        assert!(identity(&verbatim, "auto.txt").is_some());
        assert_eq!(identity(&verbatim, "crlf.txt"), None);
        assert_eq!(identity(&verbatim, "filtered.txt"), None);
        assert!(identity(&input, "plain.txt").is_some());
        assert_ne!(
            identity(&verbatim, "plain.txt"),
            identity(&input, "plain.txt"),
            "configuration is not in the identity"
        );
        assert_eq!(identity(&crlf_everywhere, "plain.txt"), None);
    }

    /// The promise an identity makes, checked over every combination of the
    /// inputs to git's conversion: a path git's checkout transforms must
    /// carry none. A line or a setting missing here is a combination the
    /// classifier answers for unchecked.
    #[test]
    fn an_identity_is_only_given_where_git_checks_out_the_blob_unchanged() {
        const ATTRIBUTE_LINES: &[&str] = &[
            "",
            "* text",
            "* -text",
            "* text=auto",
            "* text=input",
            "* binary",
            "* eol=lf",
            "* eol=crlf",
            "* text eol=lf",
            "* text eol=crlf",
            "* text=auto eol=lf",
            "* text=auto eol=crlf",
            "* crlf",
            "* -crlf",
            "* crlf=input",
            "* ident",
            "* filter=upper",
            "* working-tree-encoding=ISO-8859-1",
        ];
        const AUTOCRLF: &[Option<&str>] = &[None, Some("false"), Some("true"), Some("input")];
        const EOL: &[Option<&str>] = &[None, Some("lf"), Some("crlf"), Some("native")];
        let files: &[(&str, &[u8])] = &[
            ("lf.txt", "one\ntwo $Id$ \u{e9}\n".as_bytes()),
            ("crlf.txt", b"one\r\ntwo\r\n"),
        ];

        struct Outcome {
            line: &'static str,
            autocrlf: Option<&'static str>,
            eol: Option<&'static str>,
            shareable: bool,
            unchanged: bool,
        }

        let outcomes: Vec<Outcome> = std::thread::scope(|s| {
            let workers: Vec<_> = ATTRIBUTE_LINES
                .iter()
                .map(|&line| {
                    s.spawn(move || {
                        let (_tmp, repo) =
                            repo_with(files, &format!("{line}\n"), LineEndings::Verbatim);
                        let repo = repo.as_path();
                        git(repo, &["config", "filter.upper.smudge", "tr a-z A-Z"]);
                        git(repo, &["config", "filter.upper.clean", "cat"]);
                        let dest = unpopulated(repo, "w");
                        let mut outcomes = Vec::new();
                        for &autocrlf in AUTOCRLF {
                            for &eol in EOL {
                                for (key, value) in [("core.autocrlf", autocrlf), ("core.eol", eol)]
                                {
                                    let _ = git_command()
                                        .args(["config", "--unset", key])
                                        .current_dir(repo)
                                        .output();
                                    if let Some(value) = value {
                                        git(repo, &["config", key, value]);
                                    }
                                }
                                for (name, _) in files {
                                    let _ = std::fs::remove_file(dest.join(name));
                                }
                                git(&dest, &["checkout-index", "-a", "-f"]);
                                let tracked = by_path(population().tracked_files(&dest).unwrap());
                                for (name, _) in files {
                                    let written = std::fs::read(dest.join(name)).unwrap();
                                    let unchanged = written == blob_bytes(repo, name);
                                    let shareable = tracked[*name].identity().is_some();
                                    assert!(
                                        unchanged || !shareable,
                                        "{name} under {line:?}, core.autocrlf={autocrlf:?}, \
                                         core.eol={eol:?}: carries an identity but git's \
                                         checkout changed its bytes"
                                    );
                                    outcomes.push(Outcome {
                                        line,
                                        autocrlf,
                                        eol,
                                        shareable,
                                        unchanged,
                                    });
                                }
                            }
                        }
                        outcomes
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap())
                .collect()
        });

        let shareable = outcomes.iter().filter(|o| o.shareable).count();
        let converted = outcomes.iter().filter(|o| !o.unchanged).count();
        assert_eq!(
            outcomes.len(),
            ATTRIBUTE_LINES.len() * AUTOCRLF.len() * EOL.len() * files.len()
        );
        assert!(
            shareable > 0 && converted > 0,
            "the matrix exercised one side only: {shareable} shareable, {converted} converted"
        );
        let all_shared = |pick: &dyn Fn(&Outcome) -> bool| {
            let picked: Vec<_> = outcomes.iter().filter(|o| pick(o)).collect();
            !picked.is_empty() && picked.iter().all(|o| o.shareable)
        };
        assert!(
            all_shared(&|o| o.line.is_empty()
                && o.autocrlf == Some("false")
                && o.eol == Some("lf")),
            "a repo with no conversion shares nothing"
        );
        assert!(
            outcomes
                .iter()
                .filter(|o| o.line.is_empty() && o.autocrlf == Some("true"))
                .all(|o| !o.shareable),
            "Git for Windows' default shares an undeclared file"
        );
        assert!(all_shared(&|o| o.line == "* text=auto eol=lf"));
        assert!(all_shared(&|o| o.line == "* -text"));
    }

    fn holding_recorded_content_is_byte_exact(endings: LineEndings) {
        let (_tmp, repo) = repo_with(
            &[
                ("plain.txt", b"plain\n"),
                ("plain.bin", b"plain\n"),
                ("auto.txt", b"one\ntwo\n"),
                ("edited.bin", b"edited\n"),
                ("touched.bin", b"touched\n"),
                ("gone.bin", b"gone\n"),
                ("filtered.txt", b"filtered\n"),
                ("unasked.bin", b"unasked\n"),
            ],
            "*.bin -text\nauto.txt text=auto\nfiltered.txt filter=upper\n",
            endings,
        );
        let repo = repo.as_path();
        let w = checked_out(repo, "w");

        std::fs::write(w.join("auto.txt"), b"one\r\ntwo\r\n").unwrap();
        git(&w, &["read-tree", "HEAD"]);
        git(&w, &["update-index", "-q", "--refresh"]);
        assert!(
            !GIT.tracked_dirty_file_names(&w)
                .unwrap()
                .contains(&"auto.txt".to_owned()),
            "git no longer reads a CRLF rewrite of a text=auto file as unchanged"
        );
        std::fs::write(w.join("edited.bin"), b"EDITED\n").unwrap();
        let touched = std::fs::File::options()
            .write(true)
            .open(w.join("touched.bin"))
            .unwrap();
        touched
            .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        drop(touched);
        assert!(
            git(&w, &["diff-files", "--name-only"]).contains("touched.bin"),
            "the touched file is not stat-dirty, so this case tests nothing"
        );
        std::fs::remove_file(w.join("gone.bin")).unwrap();

        let asked = paths(&[
            "plain.txt",
            "plain.bin",
            "auto.txt",
            "edited.bin",
            "touched.bin",
            "gone.bin",
            "filtered.txt",
            "not-tracked.txt",
        ]);
        let holding: Vec<String> = population()
            .files_holding_recorded_content(&w, &asked)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();

        let expected: &[&str] = match endings {
            LineEndings::Verbatim => &["plain.bin", "plain.txt", "touched.bin"],
            LineEndings::Converted => &["plain.bin", "touched.bin"],
        };
        assert_eq!(holding, paths(expected));
    }

    #[cfg(unix)]
    fn holding_recorded_content_needs_a_regular_file_with_the_recorded_executable_bit(
        endings: LineEndings,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, repo) = repo_with(
            &[
                ("run.sh", b"#!/bin/sh\n"),
                ("flipped.sh", b"#!/bin/sh\n"),
                ("linked.txt", b"same\n"),
                ("\"quoted", b"q\n"),
                ("new\nline", b"n\n"),
                ("ends\r", b"r\n"),
            ],
            "*.sh text eol=lf\n",
            endings,
        );
        let repo = repo.as_path();
        git(
            repo,
            &["update-index", "--chmod=+x", "run.sh", "flipped.sh"],
        );
        git(repo, &["commit", "-q", "-m", "executable"]);
        let w = checked_out(repo, "w");
        std::fs::set_permissions(w.join("flipped.sh"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        std::fs::write(w.join("elsewhere.txt"), b"same\n").unwrap();
        std::fs::remove_file(w.join("linked.txt")).unwrap();
        std::os::unix::fs::symlink("elsewhere.txt", w.join("linked.txt")).unwrap();

        let asked = paths(&[
            "run.sh",
            "flipped.sh",
            "linked.txt",
            "\"quoted",
            "new\nline",
            "ends\r",
        ]);
        let holding: Vec<String> = population()
            .files_holding_recorded_content(&w, &asked)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();

        let expected: &[&str] = match endings {
            LineEndings::Verbatim => &["\"quoted", "ends\r", "new\nline", "run.sh"],
            LineEndings::Converted => &["run.sh"],
        };
        assert_eq!(holding, paths(expected));
    }

    fn finishing_writes_only_what_is_missing_and_leaves_a_checkout(endings: LineEndings) {
        let (_tmp, repo) = repo_with(
            &[
                ("a.txt", b"a\n"),
                ("b.txt", b"b\n"),
                ("run.sh", b"#!/bin/sh\n"),
            ],
            "",
            endings,
        );
        let repo = repo.as_path();
        git(repo, &["update-index", "--chmod=+x", "run.sh"]);
        git(repo, &["commit", "-q", "-m", "executable"]);
        commit_link_and_nested(repo);
        let source = checked_out(repo, "source");
        let dest = unpopulated(repo, "w");
        std::fs::hard_link(source.join("a.txt"), dest.join("a.txt")).unwrap();

        population().finish_checkout(&dest).unwrap();

        assert!(GIT.tracked_dirty_file_names(&dest).unwrap().is_empty());
        assert_eq!(git(&dest, &["symbolic-ref", "HEAD"]), "refs/heads/w");
        assert_eq!(git(&dest, &["rev-parse", "HEAD"]), head(repo).id().as_str());
        for name in ["a.txt", "b.txt", "run.sh"] {
            assert_eq!(
                std::fs::read(dest.join(name)).unwrap(),
                checked_out_bytes(&dest, name),
                "{name} is not what git's checkout writes"
            );
        }
        assert!(dest.join("nested").is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let inode = |p: &Path| std::fs::metadata(p).unwrap().ino();
            assert_eq!(
                inode(&dest.join("a.txt")),
                inode(&source.join("a.txt")),
                "finishing rewrote a placed file"
            );
            let mode = std::fs::metadata(dest.join("run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o100, 0);
            assert_eq!(
                std::fs::read_link(dest.join("link")).unwrap(),
                Path::new("a.txt")
            );
        }
    }

    fn record_post_checkout_to(repo: &Path, exit: u8) -> PathBuf {
        let hooks = repo.join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        let log = repo.join("post-checkout.log");
        let script = hooks.join("post-checkout");
        crate::write_executable(&script, format!(
                "#!/bin/sh\necho \"$* $(basename \"$(pwd)\")\" >> '{}'\necho refused-by-hook >&2\nexit {exit}\n",
                log.display()
            ));

        git(repo, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        log
    }

    #[test]
    fn finishing_runs_the_post_checkout_hook_as_worktree_add_does() {
        let (_tmp, repo) = repo_with(&[("a.txt", b"a\n")], "", LineEndings::Verbatim);
        let repo = repo.as_path();
        let log = record_post_checkout_to(repo, 0);

        checked_out(repo, "plain");
        let populated = unpopulated(repo, "populated");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "creating without files ran the hook early"
        );
        population().finish_checkout(&populated).unwrap();

        let runs: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let head = head(repo);
        let null = "0".repeat(head.id().as_str().len());
        assert_eq!(
            runs,
            vec![
                format!("{null} {head} 1 plain"),
                format!("{null} {head} 1 populated"),
            ]
        );
    }

    #[test]
    fn a_failing_post_checkout_hook_rejects_the_finish() {
        let (_tmp, repo) = repo_with(&[("a.txt", b"a\n")], "", LineEndings::Verbatim);
        let repo = repo.as_path();
        let dest = unpopulated(repo, "w");
        record_post_checkout_to(repo, 1);

        let err = population().finish_checkout(&dest).unwrap_err();

        match err {
            VcsError::HookRejected { stderr, .. } => assert!(stderr.contains("refused-by-hook")),
            other => panic!("expected a hook rejection, got {other:?}"),
        }
    }

    fn checking_out_a_path_replaces_it_and_leaves_what_shared_it_alone(endings: LineEndings) {
        let (_tmp, repo) = repo_with(
            &[("a.bin", b"a\n"), ("b.bin", b"b\n"), ("c.txt", b"c\n")],
            "*.bin -text\n",
            endings,
        );
        let repo = repo.as_path();
        let source = checked_out(repo, "source");
        let dest = unpopulated(repo, "w");
        std::fs::hard_link(source.join("a.bin"), dest.join("a.bin")).unwrap();
        population().finish_checkout(&dest).unwrap();
        std::fs::write(source.join("a.bin"), b"changed under the share\n").unwrap();
        std::fs::write(dest.join("c.txt"), b"edited\n").unwrap();
        let b_before = std::fs::read(dest.join("b.bin")).unwrap();
        assert!(population()
            .files_holding_recorded_content(&dest, &paths(&["a.bin"]))
            .unwrap()
            .is_empty());

        population()
            .check_out_paths(&dest, &paths(&["a.bin", "c.txt"]))
            .unwrap();

        for name in ["a.bin", "c.txt"] {
            assert_eq!(
                std::fs::read(dest.join(name)).unwrap(),
                checked_out_bytes(&dest, name),
                "{name} is not what git's checkout writes"
            );
        }
        assert_eq!(
            std::fs::read(source.join("a.bin")).unwrap(),
            b"changed under the share\n",
            "the check-out wrote through the shared file"
        );
        assert_eq!(std::fs::read(dest.join("b.bin")).unwrap(), b_before);
        let shareable = match endings {
            LineEndings::Verbatim => 3,
            LineEndings::Converted => 2,
        };
        assert_eq!(
            population()
                .files_holding_recorded_content(&dest, &paths(&["a.bin", "b.bin", "c.txt"]))
                .unwrap()
                .len(),
            shareable
        );
    }

    #[test]
    fn a_mode_git_does_not_record_in_an_index_does_not_parse() {
        let recorded = [
            ("100644", FileMode::Regular),
            ("100755", FileMode::Executable),
            ("120000", FileMode::Symlink),
            ("160000", FileMode::Gitlink),
        ];
        for (token, mode) in recorded {
            assert_eq!(FileMode::parse(token), Some(mode), "{token}");
        }
        for token in ["100664", "040000", "0", "", "100644 "] {
            assert_eq!(FileMode::parse(token), None, "{token:?}");
        }
        assert!(FileMode::Regular.is_regular_file() && FileMode::Executable.is_regular_file());
        assert!(!FileMode::Symlink.is_regular_file() && !FileMode::Gitlink.is_regular_file());
    }

    #[test]
    fn checking_out_an_untracked_path_is_refused() {
        let (_tmp, repo) = repo_with(&[("a.txt", b"a\n")], "", LineEndings::Verbatim);
        let dest = unpopulated(&repo, "w");

        assert!(population()
            .check_out_paths(&dest, &paths(&["nope.txt"]))
            .is_err());
    }

    under_both_line_endings!(
        tracked_files_are_the_tree_with_its_kinds_and_identities_only_on_files,
        holding_recorded_content_is_byte_exact,
        #[cfg(unix)]
        holding_recorded_content_needs_a_regular_file_with_the_recorded_executable_bit,
        finishing_writes_only_what_is_missing_and_leaves_a_checkout,
        checking_out_a_path_replaces_it_and_leaves_what_shared_it_alone,
    );
}

#[cfg(test)]
mod peer_registration_tests {
    //! `git worktree add` creates the new worktree's registration under the
    //! shared repo and writes its `commondir` a moment later. A git command
    //! that enumerates the repo's worktrees in between reads the file empty
    //! and dies with "failed to read .../commondir". The window is
    //! microseconds, so these tests hold it open: a registration whose
    //! `commondir` is truncated, and a thread that finishes it later.

    use super::*;
    use crate::path_spelling::resolve;
    use std::time::Duration;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = git_command_in_test()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A repo on `main` holding two registered worktrees, `other` and `peer`.
    struct Fixture {
        _tmp: tempfile::TempDir,
        store: PathBuf,
        base: String,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = crate::tempdir().unwrap();
            let store = tmp.path().join("store");
            std::fs::create_dir(&store).unwrap();
            git(&store, &["init", "-b", "main"]);
            git(&store, &["config", "user.email", "t@t"]);
            git(&store, &["config", "user.name", "T"]);
            git(&store, &["commit", "--allow-empty", "-m", "one"]);
            for name in ["other", "peer"] {
                let path = tmp.path().join(name);
                git(
                    &store,
                    &["worktree", "add", "-b", name, path.to_str().unwrap()],
                );
            }
            let base = git(&store, &["rev-parse", "HEAD"]);
            Self {
                _tmp: tmp,
                store,
                base,
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self._tmp.path().join(name)
        }

        fn commondir_of_peer(&self) -> PathBuf {
            self.store
                .join(".git")
                .join("worktrees")
                .join("peer")
                .join("commondir")
        }

        /// How rwv names that file: under git's common directory, in the one
        /// spelling a resolved path has.
        fn named_commondir_of_peer(&self) -> PathBuf {
            resolve(&self.store.join(".git"))
                .unwrap()
                .join("worktrees")
                .join("peer")
                .join("commondir")
        }

        /// Leave `peer`'s registration as `git worktree add` does for a
        /// moment: created, `commondir` empty. The returned thread finishes it
        /// after `after`.
        fn leave_peer_unwritten(&self, after: Duration) -> std::thread::JoinHandle<()> {
            let commondir = self.commondir_of_peer();
            let written = std::fs::read(&commondir).unwrap();
            std::fs::write(&commondir, b"").unwrap();
            std::thread::spawn(move || {
                std::thread::sleep(after);
                std::fs::write(&commondir, written).unwrap();
            })
        }
    }

    const LATER: Duration = Duration::from_millis(200);

    #[cfg(unix)]
    #[test]
    fn registrations_that_cannot_be_listed_are_an_error_not_an_absence() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new();
        let registrations = fx.store.join(".git").join("worktrees");
        let found = GitVcs.worktree_registration(&fx.store, &fx.path("other"));
        assert!(found.unwrap().is_some());

        std::fs::set_permissions(&registrations, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&registrations).is_ok() {
            std::fs::set_permissions(&registrations, std::fs::Permissions::from_mode(0o755))
                .unwrap();
            return;
        }
        let found = GitVcs.worktree_registration(&fx.store, &fx.path("other"));
        std::fs::set_permissions(&registrations, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(found.is_err(), "{found:?}");
    }

    fn listing_worktrees_waits_for_a_peer_to_finish_its_registration() {
        let fx = Fixture::new();
        let finishing = fx.leave_peer_unwritten(LATER);

        let listed = GitVcs.list_worktrees(&fx.store);
        finishing.join().unwrap();

        let listed = listed.expect("the peer finished within the wait");
        assert_eq!(listed.len(), 3, "{listed:?}");
    }

    fn removing_a_worktree_waits_for_a_peer_to_finish_its_registration() {
        let fx = Fixture::new();
        let finishing = fx.leave_peer_unwritten(LATER);

        let removed = GitVcs.remove_worktree(&fx.store, &fx.path("other"));
        finishing.join().unwrap();

        removed.expect("the peer finished within the wait");
        assert!(!fx.path("other").exists());
    }

    fn destroying_a_branch_waits_for_a_peer_to_finish_its_registration() {
        let fx = Fixture::new();
        git(&fx.store, &["branch", "spare"]);
        let finishing = fx.leave_peer_unwritten(LATER);

        let head = GitVcs.head_revision(&fx.store).unwrap();
        let proof = DestroyProof::landed(
            &GitVcs,
            &fx.store,
            Site::Local,
            &RawRefName::new("spare"),
            &[head],
        )
        .unwrap()
        .expect("spare sits where the repo's head does");
        let destroyed = GitVcs.delete_branch(proof);
        finishing.join().unwrap();

        destroyed.expect("the peer finished within the wait");
        assert!(GitVcs
            .resolve_local_branch_tip(&fx.store, &RawRefName::new("spare"))
            .unwrap()
            .is_none());
    }

    /// git creates the branch before it reads the registrations, so the
    /// attempt that dies has already authored it; the call is still the one
    /// that authored it, and the worktree ends on it.
    /// Peers that keep starting registrations while a command keeps losing to
    /// them: a failure the look finds clean is asked again, and only two in a
    /// row with nothing unfinished seen between are believed.
    fn clean_looks_count_only_while_nothing_unfinished_was_seen_between_them() {
        let fx = Fixture::new();
        let commondir = fx.commondir_of_peer();
        let written = std::fs::read(&commondir).unwrap();
        let attempts = std::cell::Cell::new(0);
        let lost = || VcsError::CommandFailed {
            args: vec!["worktree".to_owned(), "list".to_owned()],
            repo: fx.store.clone(),
            stderr: "fatal: failed to read x/commondir: Success".to_owned(),
        };

        let outcome =
            GitVcs::awaiting_peer_registration(&fx.store, None, Duration::from_secs(5), || {
                attempts.set(attempts.get() + 1);
                match attempts.get() {
                    // Lost to a peer still writing, then the peer finishes.
                    1 | 3 => {
                        std::fs::write(&commondir, b"").unwrap();
                        Err(lost())
                    }
                    2 | 4 => {
                        std::fs::write(&commondir, &written).unwrap();
                        Err(lost())
                    }
                    _ => Ok(()),
                }
            });

        outcome.expect("each clean look was separated from the next by an unfinished one");
        assert_eq!(attempts.get(), 5);
    }

    fn creating_a_worktree_on_a_new_branch_still_authors_it_after_waiting() {
        let fx = Fixture::new();
        let start = GitVcs.head_revision(&fx.store).unwrap();
        let finishing = fx.leave_peer_unwritten(LATER);

        let dest = fx.path("new");
        let origin =
            GitVcs.materialize_worktree_on_ref(&fx.store, &dest, &RawRefName::new("p--w"), &start);
        finishing.join().unwrap();

        assert_eq!(origin.unwrap(), RefOrigin::Authored);
        assert_eq!(git(&dest, &["rev-parse", "HEAD"]), fx.base);
        assert_eq!(git(&dest, &["symbolic-ref", "--short", "HEAD"]), "p--w");
    }

    #[test]
    fn a_registration_that_stays_unwritten_fails_with_gits_error_naming_the_file() {
        let fx = Fixture::new();
        std::fs::write(fx.commondir_of_peer(), b"").unwrap();

        let started = std::time::Instant::now();
        let err =
            GitVcs::awaiting_peer_registration(&fx.store, None, Duration::from_millis(80), || {
                GitVcs::run(["worktree", "list", "--porcelain"], &fx.store)
            })
            .expect_err("a registration nobody finishes stays a failure");

        assert!(started.elapsed() >= Duration::from_millis(80));
        let VcsError::CommandFailed { stderr, .. } = err else {
            panic!("expected git's own failure, got {err:?}");
        };
        assert!(stderr.starts_with("fatal: failed to read"), "{stderr}");
        assert!(
            stderr.contains(&fx.named_commondir_of_peer().display().to_string()),
            "the file that stayed empty is named: {stderr}"
        );
    }

    fn a_failure_that_is_not_an_unwritten_registration_is_not_waited_on() {
        let fx = Fixture::new();
        let attempts = std::cell::Cell::new(0);

        let unrelated =
            GitVcs::awaiting_peer_registration(&fx.store, None, Duration::from_secs(5), || {
                attempts.set(attempts.get() + 1);
                GitVcs::run(["rev-parse", "--verify", "no-such-ref"], &fx.store)
            });
        assert!(unrelated.is_err());
        assert_eq!(attempts.get(), 1);
    }

    fn creating_a_worktree_on_an_existing_branch_still_adopts_it_after_waiting() {
        let fx = Fixture::new();
        let start = GitVcs.head_revision(&fx.store).unwrap();
        git(&fx.store, &["branch", "p--w"]);
        let finishing = fx.leave_peer_unwritten(LATER);

        let dest = fx.path("new");
        let origin =
            GitVcs.materialize_worktree_on_ref(&fx.store, &dest, &RawRefName::new("p--w"), &start);
        finishing.join().unwrap();

        assert_eq!(origin.unwrap(), RefOrigin::Adopted);
        assert_eq!(git(&dest, &["symbolic-ref", "--short", "HEAD"]), "p--w");
    }

    in_both_languages!(
        listing_worktrees_waits_for_a_peer_to_finish_its_registration,
        removing_a_worktree_waits_for_a_peer_to_finish_its_registration,
        destroying_a_branch_waits_for_a_peer_to_finish_its_registration,
        clean_looks_count_only_while_nothing_unfinished_was_seen_between_them,
        creating_a_worktree_on_a_new_branch_still_authors_it_after_waiting,
        a_failure_that_is_not_an_unwritten_registration_is_not_waited_on,
        creating_a_worktree_on_an_existing_branch_still_adopts_it_after_waiting,
    );
}

#[cfg(test)]
mod log_tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = git_command_in_test()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(repo: &Path, name: &str) -> CommitId {
        std::fs::write(repo.join(name), name).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-m", name]);
        GitVcs.head_revision(repo).unwrap().id().clone()
    }

    /// `main` at `base`, then `feat` one commit past it and `main` one commit
    /// past it, with HEAD on `feat`.
    fn diverged() -> (tempfile::TempDir, CommitId, CommitId, CommitId) {
        let tmp = crate::tempdir().unwrap();
        let p = tmp.path();
        git(p, &["init", "-b", "main"]);
        git(p, &["config", "user.email", "t@t"]);
        git(p, &["config", "user.name", "T"]);
        let base = commit(p, "base");
        git(p, &["branch", "feat"]);
        let main_tip = commit(p, "main-work");
        git(p, &["switch", "feat"]);
        let feat_tip = commit(p, "feat-work");
        (tmp, base, main_tip, feat_tip)
    }

    fn subjects(commits: &[CommitSummary]) -> Vec<&str> {
        commits.iter().map(|c| c.subject.as_str()).collect()
    }

    #[test]
    fn the_endpoints_of_a_range_are_its_spelled_sides_less_an_empty_one() {
        assert_eq!(range_endpoints("a..b"), ["a", "b"]);
        assert_eq!(range_endpoints("a...b"), ["a", "b"]);
        assert_eq!(range_endpoints("a.."), ["a"]);
        assert_eq!(range_endpoints("..b"), ["b"]);
        assert_eq!(range_endpoints("^a"), ["a"]);
        assert_eq!(range_endpoints("a"), ["a"]);
        assert_eq!(range_endpoints("v1.2"), ["v1.2"]);
    }

    #[test]
    fn log_lists_a_range_newest_first_with_the_commit_each_endpoint_names() {
        let (tmp, _base, main_tip, feat_tip) = diverged();

        let listing = GitVcs.log(tmp.path(), Some("main..feat")).unwrap();

        assert_eq!(subjects(&listing.commits), ["feat-work"]);
        let endpoints: Vec<(&str, &CommitId)> = listing
            .endpoints
            .iter()
            .map(|e| (e.spelled.as_str(), &e.id))
            .collect();
        assert_eq!(endpoints, [("main", &main_tip), ("feat", &feat_tip)]);
    }

    #[test]
    fn log_with_no_range_is_the_log_of_head() {
        let (tmp, _base, _main_tip, _feat_tip) = diverged();

        let listing = GitVcs.log(tmp.path(), None).unwrap();

        assert_eq!(subjects(&listing.commits), ["feat-work", "base"]);
        assert!(listing.endpoints.is_empty());
    }

    #[test]
    fn log_names_the_endpoint_that_resolves_to_nothing_as_spelled() {
        let (tmp, ..) = diverged();

        match GitVcs.log(tmp.path(), Some("main..nosuch")) {
            Err(VcsError::RevisionNotFound { rev, .. }) => assert_eq!(rev, "nosuch"),
            other => panic!("expected RevisionNotFound, got {other:?}"),
        }
    }

    #[test]
    fn log_does_not_read_a_spelling_git_would_take_for_an_option() {
        let (tmp, ..) = diverged();

        for spelled in ["--all", "main..--all", "-1"] {
            match GitVcs.log(tmp.path(), Some(spelled)) {
                Err(VcsError::RevisionNotFound { rev, .. }) => {
                    assert!(spelled.contains(&rev), "{spelled}: {rev}");
                }
                other => panic!("{spelled}: expected RevisionNotFound, got {other:?}"),
            }
        }
    }

    #[test]
    fn log_does_not_read_a_file_named_like_the_endpoint_as_a_revision() {
        let (tmp, ..) = diverged();
        std::fs::write(tmp.path().join("nosuch"), "a file").unwrap();

        assert!(matches!(
            GitVcs.log(tmp.path(), Some("nosuch")),
            Err(VcsError::RevisionNotFound { .. })
        ));
    }

    #[test]
    fn commits_beyond_excludes_what_the_base_already_has() {
        let (tmp, _base, main_tip, feat_tip) = diverged();
        let main = GitVcs.resolve_revision(tmp.path(), "main").unwrap();
        let feat = GitVcs.resolve_revision(tmp.path(), "feat").unwrap();

        let beyond = GitVcs.commits_beyond(tmp.path(), &feat, &main).unwrap();

        assert_eq!(subjects(&beyond), ["feat-work"]);
        assert_eq!(beyond[0].id, feat_tip);
        assert_ne!(beyond[0].id, main_tip);
    }

    #[test]
    fn diff_beyond_starts_at_the_merge_base_not_at_the_base() {
        let (tmp, base, _main_tip, _feat_tip) = diverged();
        let main = GitVcs.resolve_revision(tmp.path(), "main").unwrap();

        let beyond = GitVcs.diff_beyond(tmp.path(), &main).unwrap();

        assert_eq!(beyond.base, Some(base));
        assert!(beyond.text.contains("feat-work"), "{}", beyond.text);
        assert!(
            !beyond.text.contains("main-work"),
            "work the base gained after the fork is not a reversal:\n{}",
            beyond.text
        );
    }
}

#[cfg(test)]
mod remote_role_tests {
    //! The roles a remote plays, resolved from a repo's own configuration:
    //! its url remote, the project repo's, where a branch publishes, where a
    //! new branch is taken from, and what a branch takes in from.

    use super::*;
    use crate::vcs::{Relation, VcsErrorKind};

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = git_command()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn repo() -> tempfile::TempDir {
        let tmp = crate::tempdir().unwrap();
        git(tmp.path(), &["init", "-b", "main"]);
        git(tmp.path(), &["config", "user.email", "t@t"]);
        git(tmp.path(), &["config", "user.name", "T"]);
        commit(tmp.path(), "one");
        tmp
    }

    fn commit(dir: &Path, name: &str) -> ResolvedRevisionId {
        std::fs::write(dir.join(name), name).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", name]);
        GitVcs.head_revision(dir).unwrap()
    }

    fn url(text: &str) -> CloneUrl {
        let source: crate::manifest::RepoUrl = text.parse().unwrap();
        crate::registry::resolve_to_clone_info(&source).unwrap().url
    }

    fn attached(dir: &Path) -> AttachedRef {
        let HeadAttachment::Attached(a) = GitVcs.head_attachment(dir).unwrap() else {
            panic!("fixture should be attached");
        };
        a
    }

    fn names(dir: &Path) -> Vec<String> {
        git(dir, &["remote"]).lines().map(str::to_owned).collect()
    }

    fn handle(dir: &Path, name: &str) -> RemoteHandle {
        GitVcs
            .recorded_urls(dir)
            .unwrap()
            .into_iter()
            .map(|r| r.remote)
            .find(|r| r.name() == name)
            .unwrap_or_else(|| panic!("no remote {name}"))
    }

    #[test]
    fn the_owner_segment_is_the_segment_before_the_repository_whatever_the_scheme() {
        for (spelled, owner) in [
            ("https://github.com/acme/lib.git", Some("acme")),
            ("https://github.com/acme/lib", Some("acme")),
            ("git@github.com:acme/lib.git", Some("acme")),
            ("ssh://git@host.example/acme/lib.git", Some("acme")),
            ("file:///srv/git/team/lib.git", Some("team")),
            ("/srv/git/team/lib.git", Some("team")),
            ("https://host.example/lib.git", None),
            ("https://host.example/odd name/lib.git", Some("odd-name")),
        ] {
            assert_eq!(owner_segment(spelled).as_deref(), owner, "{spelled}");
        }
    }

    #[test]
    fn the_url_remote_is_the_remote_that_has_the_url_however_it_is_spelled() {
        let repo = repo();
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "mirror",
                "https://host.example/acme/lib.git",
            ],
        );
        git(
            repo.path(),
            &["remote", "add", "main-line", "git@github.com:acme/lib.git"],
        );

        let found = GitVcs
            .url_remote(repo.path(), &url("https://github.com/acme/lib"))
            .unwrap();

        assert!(
            matches!(&found, UrlRemote::Present(r) if r.name() == "main-line"),
            "{found:?}"
        );
    }

    #[test]
    fn a_missing_url_remote_is_named_by_the_owner_and_nothing_is_added() {
        let repo = repo();
        git(
            repo.path(),
            &["remote", "add", "fork", "https://host.example/me/lib.git"],
        );

        let found = GitVcs
            .url_remote(repo.path(), &url("https://github.com/acme/lib.git"))
            .unwrap();

        let UrlRemote::Absent(absent) = found else {
            panic!("expected an absent remote, got {found:?}");
        };
        assert_eq!(absent.remote().name(), "acme");
        assert_eq!(names(repo.path()), ["fork"], "finding adds nothing");

        let added = GitVcs.add_url_remote(repo.path(), &absent).unwrap();
        assert_eq!(added.name(), "acme");
        assert_eq!(
            git(repo.path(), &["config", "remote.acme.url"]),
            "https://github.com/acme/lib.git"
        );
    }

    #[test]
    fn the_first_remote_of_a_repo_takes_gits_conventional_name() {
        let repo = repo();

        let found = GitVcs
            .url_remote(repo.path(), &url("https://github.com/acme/lib.git"))
            .unwrap();

        assert!(
            matches!(&found, UrlRemote::Absent(a) if a.remote().name() == "origin"),
            "{found:?}"
        );
    }

    #[test]
    fn a_name_that_holds_another_url_is_taken_and_nothing_is_added() {
        let repo = repo();
        git(
            repo.path(),
            &["remote", "add", "origin", "https://host.example/me/lib.git"],
        );
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "acme",
                "https://elsewhere.example/other/lib.git",
            ],
        );

        let found = GitVcs
            .url_remote(repo.path(), &url("https://github.com/acme/lib.git"))
            .unwrap();

        let UrlRemote::NameTaken(taken) = found else {
            panic!("expected a taken name, got {found:?}");
        };
        assert_eq!(taken.remote.name(), "acme");
        assert_eq!(taken.holds, "https://elsewhere.example/other/lib.git");
        assert_eq!(names(repo.path()), ["acme", "origin"]);
    }

    #[test]
    fn the_project_url_remote_is_the_recorded_remote_else_the_only_one_else_none() {
        let repo = repo();
        assert!(GitVcs.project_url_remote(repo.path()).unwrap().is_none());

        git(
            repo.path(),
            &["remote", "add", "first", "file:///nowhere/a.git"],
        );
        let only = GitVcs.project_url_remote(repo.path()).unwrap();
        assert_eq!(only.unwrap().name(), "first", "its only remote");

        git(
            repo.path(),
            &["remote", "add", "second", "file:///nowhere/b.git"],
        );
        assert!(
            GitVcs.project_url_remote(repo.path()).unwrap().is_none(),
            "two remotes and none recorded is a choice nobody made"
        );

        GitVcs
            .record_project_url_remote(repo.path(), &handle(repo.path(), "second"))
            .unwrap();
        let recorded = GitVcs.project_url_remote(repo.path()).unwrap();
        assert_eq!(recorded.unwrap().name(), "second");

        git(repo.path(), &["remote", "remove", "second"]);
        let stale = GitVcs.project_url_remote(repo.path()).unwrap();
        assert_eq!(
            stale.unwrap().name(),
            "first",
            "a recorded remote the repo no longer has counts as none recorded"
        );
    }

    #[test]
    fn the_project_url_remote_of_a_non_repo_is_an_error_not_an_absence() {
        let dir = crate::tempdir().unwrap();
        let err = GitVcs.project_url_remote(dir.path()).unwrap_err();
        assert_eq!(err.kind(), VcsErrorKind::NotARepo);
    }

    #[test]
    fn a_branch_publishes_in_gits_order_ending_with_the_url_remote() {
        let repo = repo();
        for name in ["by-branch", "by-default", "by-upstream", "by-url"] {
            git(
                repo.path(),
                &[
                    "remote",
                    "add",
                    name,
                    &format!("file:///nowhere/{name}.git"),
                ],
            );
        }
        let on = attached(repo.path());
        let url_remote = handle(repo.path(), "by-url");
        let target = |expect: Option<&str>| {
            let found = GitVcs.push_target(&on, Some(&url_remote)).unwrap();
            assert_eq!(found.as_ref().map(|r| r.name()), expect);
        };

        target(Some("by-url"));
        git(
            repo.path(),
            &["config", "branch.main.remote", "by-upstream"],
        );
        target(Some("by-upstream"));
        git(repo.path(), &["config", "remote.pushDefault", "by-default"]);
        target(Some("by-default"));
        git(
            repo.path(),
            &["config", "branch.main.pushRemote", "by-branch"],
        );
        target(Some("by-branch"));
        assert!(
            GitVcs.push_target(&on, None).unwrap().is_some(),
            "a configured remote needs no url remote"
        );
    }

    #[test]
    fn a_branch_with_no_configured_remote_and_no_url_remote_has_no_push_target() {
        let repo = repo();
        assert!(GitVcs
            .push_target(&attached(repo.path()), None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_new_branch_is_taken_from_the_configured_default_else_the_url_remote() {
        let repo = repo();
        git(
            repo.path(),
            &["remote", "add", "main-line", "file:///nowhere/m.git"],
        );
        git(
            repo.path(),
            &["remote", "add", "fork", "file:///nowhere/f.git"],
        );
        let url_remote = handle(repo.path(), "main-line");

        assert_eq!(
            GitVcs.home_remote(repo.path(), &url_remote).unwrap().name(),
            "main-line"
        );
        git(repo.path(), &["config", "checkout.defaultRemote", "fork"]);
        assert_eq!(
            GitVcs.home_remote(repo.path(), &url_remote).unwrap().name(),
            "fork"
        );
        git(
            repo.path(),
            &["config", "checkout.defaultRemote", "not-a-remote"],
        );
        assert_eq!(
            GitVcs.home_remote(repo.path(), &url_remote).unwrap().name(),
            "main-line",
            "a default that names no remote is no default"
        );
    }

    /// `repo` with a bare `remote` named `name`, `main` pushed to it, and
    /// `main` taking in from it.
    fn tracking(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let repo = repo();
        let bare = repo.path().join("remote.git");
        git(repo.path(), &["init", "--bare", "-b", "main", "remote.git"]);
        git(
            repo.path(),
            &["remote", "add", name, bare.to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", name, "main"]);
        let path = repo.path().to_path_buf();
        (repo, path, bare)
    }

    #[test]
    fn the_upstream_is_the_branch_the_vcs_records_for_taking_in() {
        let (_tmp, repo, _bare) = tracking("fork");
        let upstream = GitVcs
            .upstream(&attached(&repo))
            .unwrap()
            .expect("recorded");
        assert_eq!(upstream.remote.name(), "fork");
        assert_eq!(upstream.branch.as_str(), "main");

        git(&repo, &["switch", "-c", "feature"]);
        assert!(
            GitVcs.upstream(&attached(&repo)).unwrap().is_none(),
            "a branch with no recorded upstream has none"
        );
    }

    #[test]
    fn the_push_branch_is_the_same_named_branch_on_the_target_as_last_fetched() {
        let (_tmp, repo, bare) = tracking("fork");
        let on = attached(&repo);
        let fork = handle(&repo, "fork");

        let pushed = GitVcs.push_branch(&on, &fork).unwrap().expect("pushed");
        assert_eq!(pushed, GitVcs.head_revision(&repo).unwrap());

        git(&repo, &["switch", "-c", "feature"]);
        assert!(GitVcs
            .push_branch(&attached(&repo), &fork)
            .unwrap()
            .is_none());

        let ahead = commit(&repo, "two");
        git(&repo, &["switch", "main"]);
        assert_eq!(
            GitVcs
                .push_branch(&attached(&repo), &fork)
                .unwrap()
                .unwrap(),
            pushed,
            "main's push branch did not move with feature"
        );
        assert_ne!(ahead, pushed);
        drop(bare);
    }

    #[test]
    fn a_remote_reaches_what_one_of_its_tracking_branches_reaches() {
        let (_tmp, repo, _bare) = tracking("fork");
        git(&repo, &["remote", "add", "other", "file:///nowhere/o.git"]);
        let pushed = GitVcs.head_revision(&repo).unwrap();
        let fork = handle(&repo, "fork");
        let other = handle(&repo, "other");

        assert!(GitVcs.reached_by_remote(&repo, &pushed, &fork).unwrap());
        assert!(!GitVcs.reached_by_remote(&repo, &pushed, &other).unwrap());

        let local = commit(&repo, "two");
        assert!(!GitVcs.reached_by_remote(&repo, &local, &fork).unwrap());
    }

    #[test]
    fn relation_is_by_ancestry() {
        let repo = repo();
        let base = GitVcs.head_revision(repo.path()).unwrap();
        let tip = commit(repo.path(), "two");
        git(repo.path(), &["switch", "-c", "side", base.id().as_str()]);
        let side = commit(repo.path(), "three");

        let relate = |a: &ResolvedRevisionId, b: &ResolvedRevisionId| {
            GitVcs.relation(repo.path(), a, b).unwrap()
        };
        assert_eq!(relate(&tip, &tip), Relation::Even);
        assert_eq!(relate(&tip, &base), Relation::Ahead);
        assert_eq!(relate(&base, &tip), Relation::Behind);
        assert_eq!(relate(&tip, &side), Relation::Diverged);
    }

    #[test]
    fn unpushed_commits_are_those_no_remote_tracking_branch_reaches() {
        let (_tmp, repo, _bare) = tracking("fork");
        git(&repo, &["switch", "-c", "feature"]);
        commit(&repo, "own");
        let branch = RawRefName::new("feature");

        let unpushed = GitVcs.unpushed_commits(&repo, &branch).unwrap();
        assert_eq!(
            unpushed
                .iter()
                .map(|c| c.subject.as_str())
                .collect::<Vec<_>>(),
            ["own"]
        );

        git(&repo, &["push", "fork", "feature"]);
        assert!(GitVcs.unpushed_commits(&repo, &branch).unwrap().is_empty());
    }

    #[test]
    fn a_superseded_tip_is_not_unpushed() {
        let (_tmp, repo, _bare) = tracking("fork");
        git(&repo, &["switch", "-c", "feature"]);
        let own = commit(&repo, "own");
        git(&repo, &["push", "fork", "feature"]);
        git(
            &repo,
            &[
                "update-ref",
                &format!("{SUPERSEDED_NAMESPACE}fork/feature/{}", own.id().as_str()),
                own.id().as_str(),
            ],
        );
        git(&repo, &["push", "fork", "--delete", "feature"]);
        git(&repo, &["fetch", "--prune", "fork"]);

        assert!(
            GitVcs
                .unpushed_commits(&repo, &RawRefName::new("feature"))
                .unwrap()
                .is_empty(),
            "the commit was published once: the record says so"
        );
    }

    #[test]
    fn a_clone_returns_the_remote_it_made_whatever_the_operator_calls_a_first_remote() {
        let (_tmp, _repo, bare) = tracking("fork");
        let dest = crate::tempdir().unwrap();
        let into = dest.path().join("c");
        let config = dest.path().join("gitconfig");
        std::fs::write(&config, "[clone]\n\tdefaultRemoteName = upstream\n").unwrap();
        let out = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", &config)
            .args(["clone", bare.to_str().unwrap(), into.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success());

        let made = GitVcs::only_remote(&into).unwrap();

        assert_eq!(made.name(), "upstream");
    }
}

#[cfg(test)]
mod wording_tests {
    //! Each answer the backend draws from a git command that failed, or might
    //! have, driven through real git with git's locale pinned and again with git
    //! unpinned and speaking German. The answer comes from an exit status, a
    //! machine-readable format or the repository's state, so it is the same in
    //! both.

    use super::records::{pushed_ref, PushedRef, RECORDS_BRANCH};
    use super::*;
    use std::time::Duration;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = git_command_in_test()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn identify(repo: &Path) {
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "T"]);
    }

    /// A repo on `main` with one commit holding the file `f`.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        identify(&repo);
        std::fs::write(repo.join("f"), "content\n").unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        (tmp, repo)
    }

    fn not_a_repo() -> tempfile::TempDir {
        crate::tempdir().unwrap()
    }

    fn break_config(repo: &Path) {
        let config = repo.join(".git").join("config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("[broken\n");
        std::fs::write(config, text).unwrap();
    }

    fn at(repo: &Path, rev: &str) -> ResolvedRevisionId {
        ResolvedRevisionId::from_canonical(
            CommitId::parse(&git(repo, &["rev-parse", rev])).unwrap(),
            None,
        )
    }

    const ABSENT_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn absent_commit() -> ResolvedRevisionId {
        ResolvedRevisionId::from_canonical(CommitId::parse(ABSENT_COMMIT).unwrap(), None)
    }

    fn is_not_found(result: &Result<impl std::fmt::Debug, VcsError>) -> bool {
        matches!(result, Err(VcsError::RevisionNotFound { .. }))
    }

    fn a_file_is_read_and_its_absence_told_from_a_failure() {
        let (_tmp, repo) = repo();
        let head = at(&repo, "HEAD");

        let read = GitVcs.read_file_at_revision(&repo, &head, Path::new("f"));
        assert_eq!(read.unwrap(), "content\n");

        let no_path = GitVcs.read_file_at_revision(&repo, &head, Path::new("missing"));
        assert!(is_not_found(&no_path), "{no_path:?}");

        let no_revision = GitVcs.read_file_at_revision(&repo, &absent_commit(), Path::new("f"));
        assert!(is_not_found(&no_revision), "{no_revision:?}");

        let blob = git(&repo, &["rev-parse", "HEAD:f"]);
        let loose = repo
            .join(".git")
            .join("objects")
            .join(&blob[..2])
            .join(&blob[2..]);
        std::fs::remove_file(loose).unwrap();
        let lost = GitVcs.read_file_at_revision(&repo, &head, Path::new("f"));
        assert!(
            matches!(lost, Err(VcsError::CommandFailed { .. })),
            "a blob the store lost is a failure, not an absence: {lost:?}"
        );
    }

    fn a_revision_resolves_or_is_not_found_or_fails() {
        let (_tmp, repo) = repo();
        let head = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
        git(&repo, &["tag", "same", &head]);
        git(&repo, &["branch", "same", "HEAD"]);

        let ambiguous = GitVcs.resolve_revision(&repo, "same").unwrap();
        assert_eq!(
            ambiguous.id().as_str(),
            head,
            "git reads a tag before a branch"
        );

        let missing = GitVcs.resolve_revision(&repo, "no-such-revision");
        assert!(is_not_found(&missing), "{missing:?}");

        let elsewhere = not_a_repo();
        let failed = GitVcs.resolve_revision(elsewhere.path(), "HEAD");
        assert!(
            matches!(failed, Err(VcsError::CommandFailed { .. })),
            "{failed:?}"
        );
    }

    fn a_range_endpoint_resolves_or_is_not_found_or_fails() {
        let (_tmp, repo) = repo();
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);

        let listed = GitVcs.log(&repo, Some("HEAD~1..HEAD")).unwrap();
        assert_eq!(listed.commits.len(), 1);

        let missing = GitVcs.log(&repo, Some("no-such-revision..HEAD"));
        assert!(is_not_found(&missing), "{missing:?}");

        let elsewhere = not_a_repo();
        let failed = GitVcs.log(elsewhere.path(), Some("HEAD~1..HEAD"));
        assert!(
            matches!(failed, Err(VcsError::CommandFailed { .. })),
            "{failed:?}"
        );
    }

    fn head_is_a_commit_or_unborn_or_a_failure() {
        let (_tmp, repo) = repo();
        let head = GitVcs.head_revision(&repo).unwrap();
        assert_eq!(head.id().as_str(), git(&repo, &["rev-parse", "HEAD"]));

        let fresh = crate::tempdir().unwrap();
        git(fresh.path(), &["init", "-q", "-b", "trunk"]);
        let unborn = GitVcs.head_revision(fresh.path());
        let Err(VcsError::CommandFailed { stderr, .. }) = &unborn else {
            panic!("{unborn:?}");
        };
        assert!(stderr.contains("unborn HEAD"), "{stderr}");
        assert!(
            stderr.contains("'trunk'"),
            "the unborn branch is named: {stderr}"
        );

        let elsewhere = not_a_repo();
        let failed = GitVcs.head_revision(elsewhere.path());
        let Err(VcsError::CommandFailed { stderr, .. }) = &failed else {
            panic!("{failed:?}");
        };
        assert!(!stderr.contains("unborn HEAD"), "{stderr}");
    }

    fn a_config_key_is_set_or_unset_or_unreadable() {
        let (_tmp, repo) = repo();
        git(&repo, &["config", "branch.main.remote", "origin"]);
        git(
            &repo,
            &["config", "remote.origin.url", "https://example.invalid/r"],
        );

        assert_eq!(
            GitVcs::config_get(&repo, "branch.main.remote").unwrap(),
            Some("origin".to_owned())
        );
        assert!(GitVcs::has_branch_section(&repo, "main").unwrap());
        assert_eq!(GitVcs.recorded_urls(&repo).unwrap().len(), 1);

        let (_unset_tmp, unset) = self::repo();
        assert_eq!(
            GitVcs::config_get(&unset, "branch.main.remote").unwrap(),
            None
        );
        assert!(!GitVcs::has_branch_section(&unset, "main").unwrap());
        assert!(GitVcs.recorded_urls(&unset).unwrap().is_empty());
        assert!(GitVcs::push_target_remotes(&unset, None)
            .unwrap()
            .is_empty());

        break_config(&repo);
        assert!(GitVcs::config_get(&repo, "branch.main.remote").is_err());
        assert!(GitVcs::has_branch_section(&repo, "main").is_err());
        assert!(GitVcs.recorded_urls(&repo).is_err());
        assert!(GitVcs::push_target_remotes(&repo, None).is_err());
    }

    fn commits_share_a_base_or_none_or_the_question_fails() {
        let (_tmp, repo) = repo();
        let base = CommitId::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
        let unrelated = CommitId::parse(&git(
            &repo,
            &["commit-tree", "HEAD^{tree}", "-m", "unrelated"],
        ))
        .unwrap();

        assert_eq!(
            GitVcs::merge_base_list(&repo, &base, &base).unwrap(),
            vec![base.clone()]
        );
        assert!(GitVcs::merge_base_list(&repo, &base, &unrelated)
            .unwrap()
            .is_empty());
        let absent = CommitId::parse(ABSENT_COMMIT).unwrap();
        assert!(GitVcs::merge_base_list(&repo, &base, &absent).is_err());

        assert_eq!(
            GitVcs::merge_base_of_merge(&repo, &base, std::slice::from_ref(&base)).unwrap(),
            Some(base.clone())
        );
        assert_eq!(
            GitVcs::merge_base_of_merge(&repo, &base, std::slice::from_ref(&unrelated)).unwrap(),
            None
        );
        assert!(GitVcs::merge_base_of_merge(&repo, &base, &[absent]).is_err());
    }

    /// A bare `origin`, a clone of it publishing records, and a second clone
    /// standing for another machine.
    struct Records {
        _tmp: tempfile::TempDir,
        origin: PathBuf,
        here: PathBuf,
        there: PathBuf,
    }

    impl Records {
        fn new() -> Records {
            let (tmp, seed) = repo();
            let origin = tmp.path().join("origin.git");
            git(
                tmp.path(),
                &[
                    "clone",
                    "-q",
                    "--bare",
                    seed.to_str().unwrap(),
                    origin.to_str().unwrap(),
                ],
            );
            let clone = |name: &str| {
                let path = tmp.path().join(name);
                git(
                    tmp.path(),
                    &[
                        "clone",
                        "-q",
                        origin.to_str().unwrap(),
                        path.to_str().unwrap(),
                    ],
                );
                identify(&path);
                path
            };
            let (here, there) = (clone("here"), clone("there"));
            Records {
                _tmp: tmp,
                origin,
                here,
                there,
            }
        }

        fn commit(&self, repo: &Path, parent: Option<&CommitId>, message: &str) -> CommitId {
            let tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
            let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
            if let Some(parent) = parent {
                args.extend(["-p", parent.as_str()]);
            }
            CommitId::parse(&git(repo, &args)).unwrap()
        }

        fn publish(&self, repo: &Path, commit: &CommitId) -> Result<RecordsPublish, VcsError> {
            GitVcs::publish_records_commit(repo, &RemoteHandle::new("origin"), commit)
        }

        fn remote_tip(&self) -> String {
            git(
                &self.origin,
                &["rev-parse", &format!("refs/heads/{RECORDS_BRANCH}")],
            )
        }

        fn hook(&self, name: &str, body: &str) {
            let hook = self.origin.join("hooks").join(name);
            crate::write_executable(&hook, format!("#!/bin/sh\n{body}\n"));
        }
    }

    fn a_records_commit_is_published_where_the_remote_has_not_moved() {
        let fx = Records::new();
        let first = fx.commit(&fx.here, None, "first");
        assert_eq!(
            fx.publish(&fx.here, &first).unwrap(),
            RecordsPublish::Published
        );
        let second = fx.commit(&fx.here, Some(&first), "second");
        assert_eq!(
            fx.publish(&fx.here, &second).unwrap(),
            RecordsPublish::Published
        );
        assert_eq!(fx.remote_tip(), second.as_str());
    }

    /// Another machine published first, a commit this clone has never seen.
    fn a_records_commit_built_on_a_tip_the_remote_left_is_a_lost_race() {
        let fx = Records::new();
        let first = fx.commit(&fx.here, None, "first");
        fx.publish(&fx.here, &first).unwrap();
        git(&fx.there, &["fetch", "-q", "origin", RECORDS_BRANCH]);
        let theirs = fx.commit(&fx.there, Some(&first), "theirs");
        fx.publish(&fx.there, &theirs).unwrap();

        let ours = fx.commit(&fx.here, Some(&first), "ours");
        assert_eq!(
            fx.publish(&fx.here, &ours).unwrap(),
            RecordsPublish::RemoteMoved
        );
        assert_eq!(fx.remote_tip(), theirs.as_str());
    }

    /// The remote advertised the tip this commit was built on, then moved
    /// before swapping its ref: git reports that as the remote refusing.
    fn a_records_commit_the_remote_lost_the_swap_for_is_a_lost_race() {
        let fx = Records::new();
        let first = fx.commit(&fx.here, None, "first");
        fx.publish(&fx.here, &first).unwrap();
        let theirs = fx.commit(&fx.here, Some(&first), "theirs");
        git(
            &fx.here,
            &[
                "push",
                "-q",
                "origin",
                &format!("{}:refs/heads/elsewhere", theirs.as_str()),
            ],
        );
        fx.hook(
            "pre-receive",
            &format!(
                "unset GIT_QUARANTINE_PATH\ngit update-ref refs/heads/{RECORDS_BRANCH} {}",
                theirs.as_str()
            ),
        );

        let ours = fx.commit(&fx.here, Some(&first), "ours");
        assert_eq!(
            fx.publish(&fx.here, &ours).unwrap(),
            RecordsPublish::RemoteMoved
        );
        assert_eq!(fx.remote_tip(), theirs.as_str());
    }

    fn a_remote_refusing_the_records_branch_says_why() {
        let fx = Records::new();
        fx.hook("update", "echo this remote keeps no records >&2\nexit 1");

        let first = fx.commit(&fx.here, None, "first");
        let refused = fx.publish(&fx.here, &first).unwrap();
        let RecordsPublish::Rejected(why) = &refused else {
            panic!("{refused:?}");
        };
        assert!(why.contains("this remote keeps no records"), "{why}");
    }

    fn a_remote_that_cannot_be_reached_is_an_error() {
        let fx = Records::new();
        git(
            &fx.here,
            &[
                "remote",
                "set-url",
                "origin",
                fx.origin.join("gone").to_str().unwrap(),
            ],
        );
        let first = fx.commit(&fx.here, None, "first");
        let unreached = fx.publish(&fx.here, &first);
        assert!(
            matches!(unreached, Err(VcsError::CommandFailed { .. })),
            "{unreached:?}"
        );
    }

    /// A repo with a worktree whose `post-checkout` refuses every checkout.
    fn store_refusing_checkouts() -> (tempfile::TempDir, PathBuf) {
        let (tmp, repo) = repo();
        let hook = repo.join(".git").join("hooks").join("post-checkout");
        crate::write_executable(&hook, "#!/bin/sh\nexit 1\n");

        (tmp, repo)
    }

    /// The registration a refused `worktree add` leaves is its own, not a
    /// peer's, so the add is not made again.
    fn an_add_that_leaves_its_own_registration_is_not_repeated() {
        let (tmp, store) = store_refusing_checkouts();
        let dest = tmp.path().join("added");
        let dest_arg = dest.to_str().unwrap().to_owned();
        let attempts = std::cell::Cell::new(0);

        let added =
            GitVcs::awaiting_peer_registration(&store, Some(&dest), Duration::from_secs(5), || {
                attempts.set(attempts.get() + 1);
                GitVcs::run(["worktree", "add", "--detach", &dest_arg, "HEAD"], &store)
            });

        assert!(added.is_err(), "the hook refused the checkout");
        assert!(
            dest.join(".git").exists(),
            "the add registered its worktree"
        );
        assert_eq!(attempts.get(), 1);
    }

    /// What git said does not decide whether a peer is waited on: a failure
    /// while a registration is unwritten is waited out whatever its words, and
    /// one while none is in flight is believed at once whatever its words.
    fn the_wait_turns_on_the_registrations_not_on_what_git_said() {
        let (tmp, store) = repo();
        let peer = tmp.path().join("peer");
        git(
            &store,
            &["worktree", "add", "-q", "--detach", peer.to_str().unwrap()],
        );
        let commondir = store
            .join(".git")
            .join("worktrees")
            .join("peer")
            .join("commondir");
        let written = std::fs::read(&commondir).unwrap();
        let failure = |stderr: &str| VcsError::CommandFailed {
            args: vec!["worktree".to_owned(), "list".to_owned()],
            repo: store.clone(),
            stderr: stderr.to_owned(),
        };
        let attempts = std::cell::Cell::new(0);

        let waited =
            GitVcs::awaiting_peer_registration(&store, None, Duration::from_secs(5), || {
                attempts.set(attempts.get() + 1);
                match attempts.get() {
                    1 => {
                        std::fs::write(&commondir, b"").unwrap();
                        Err(failure("Schwerwiegend: konnte nicht lesen"))
                    }
                    2 => {
                        std::fs::write(&commondir, &written).unwrap();
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            });
        waited.expect("the unwritten registration was waited out");
        assert_eq!(attempts.get(), 2);

        attempts.set(0);
        let believed =
            GitVcs::awaiting_peer_registration(&store, None, Duration::from_secs(5), || {
                attempts.set(attempts.get() + 1);
                Err::<(), _>(failure("fatal: failed to read x/commondir: Success"))
            });
        assert!(believed.is_err());
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn a_push_status_line_is_read_by_its_flag_and_documented_kind() {
        let refname = format!("refs/heads/{RECORDS_BRANCH}");
        let line =
            |flag: &str, summary: &str| format!("To /r\n{flag}\tabc:{refname}\t{summary}\nDone\n");
        assert_eq!(
            pushed_ref(&line("*", "[new branch]"), &refname),
            Some(PushedRef::Updated)
        );
        assert_eq!(
            pushed_ref(&line(" ", "a..b"), &refname),
            Some(PushedRef::Updated)
        );
        assert_eq!(
            pushed_ref(&line("!", "[rejected] (fetch first)"), &refname),
            Some(PushedRef::Rejected)
        );
        assert_eq!(
            pushed_ref(&line("!", "[remote rejected] (hook declined)"), &refname),
            Some(PushedRef::RemoteRejected {
                summary: "[remote rejected] (hook declined)".to_owned()
            })
        );
        assert_eq!(
            pushed_ref(
                &line("!", "[remote failure] (remote failed to report status)"),
                &refname
            ),
            Some(PushedRef::Failed)
        );
        assert_eq!(
            pushed_ref(&line("*", "[new branch]"), "refs/heads/other"),
            None
        );
    }

    in_both_languages!(
        a_file_is_read_and_its_absence_told_from_a_failure,
        a_revision_resolves_or_is_not_found_or_fails,
        a_range_endpoint_resolves_or_is_not_found_or_fails,
        head_is_a_commit_or_unborn_or_a_failure,
        a_config_key_is_set_or_unset_or_unreadable,
        commits_share_a_base_or_none_or_the_question_fails,
        a_records_commit_is_published_where_the_remote_has_not_moved,
        a_records_commit_built_on_a_tip_the_remote_left_is_a_lost_race,
        a_records_commit_the_remote_lost_the_swap_for_is_a_lost_race,
        a_remote_refusing_the_records_branch_says_why,
        a_remote_that_cannot_be_reached_is_an_error,
        an_add_that_leaves_its_own_registration_is_not_repeated,
        the_wait_turns_on_the_registrations_not_on_what_git_said,
    );
}

#[cfg(test)]
mod floor_tests {
    use super::*;

    fn readiness(stdout: &str) -> ToolReadiness {
        readiness_of(&reading_of(true, stdout, ""))
    }

    #[test]
    fn a_release_is_read_from_what_vendors_append_to_it() {
        assert_eq!(release_of("git version 2.43.0\n"), Some((2, 43)));
        assert_eq!(
            release_of("git version 2.39.5 (Apple Git-154)"),
            Some((2, 39))
        );
        assert_eq!(release_of("git version 2.47.1.windows.1"), Some((2, 47)));
        assert_eq!(release_of("not git"), None);
    }

    #[test]
    fn a_release_below_the_floor_is_too_old_and_names_what_was_found() {
        assert_eq!(
            readiness("git version 2.35.8\n"),
            ToolReadiness::TooOld {
                installed: "2.35.8".to_owned()
            }
        );
        assert_eq!(
            readiness("git version 1.99.0\n"),
            ToolReadiness::TooOld {
                installed: "1.99.0".to_owned()
            }
        );
        assert_eq!(readiness("git version 2.36.0\n"), ToolReadiness::Ready);
        assert_eq!(
            readiness("git version 2.39.5 (Apple Git-154)\n"),
            ToolReadiness::Ready
        );
        assert_eq!(readiness("git version 3.0.0\n"), ToolReadiness::Ready);
    }

    #[test]
    fn a_version_naming_no_release_or_a_failed_answer_is_unreadable() {
        assert_eq!(
            readiness("hub version 2.14.2\n"),
            ToolReadiness::Unreadable {
                reported: "hub version 2.14.2".to_owned()
            }
        );
        assert_eq!(
            readiness_of(&reading_of(false, "git version 2.40.0\n", "broken")),
            ToolReadiness::Unreadable {
                reported: "git version 2.40.0\nbroken".to_owned()
            }
        );
    }

    #[test]
    fn the_git_under_test_is_at_or_above_the_floor() {
        assert_eq!(GitVcs.tool_readiness(), ToolReadiness::Ready);
    }

    fn floor() -> String {
        GitVcs.minimum_release()
    }

    fn page(path: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
    }

    #[test]
    fn the_pages_stating_the_floor_state_this_one() {
        let prerequisite = format!("**Prerequisites:** git {} or newer on `$PATH`", floor());
        for path in ["README.md", "docs/introduction.md"] {
            assert!(
                page(path).contains(&prerequisite),
                "{path} does not state {prerequisite:?}"
            );
        }
        assert!(
            page("docs/reference/cli.md").contains(&format!("Below git {} it refuses", floor()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_floor_stage_builds_this_floor() {
        let out = std::process::Command::new("bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/git-floor.sh"))
            .arg("--print-release")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("{}.0", floor())
        );
    }
}

#[cfg(test)]
mod name_reading_tests {
    //! Paths git would quote without `-z` — a space, a tab, a quote, a
    //! non-ASCII letter — read back as the paths they are.
    use super::*;

    const AWKWARD: &str = "dir with space/tab\there \"quoted\" é.txt";

    fn git(dir: &Path, args: &[&str]) {
        let out = git_command_in_test()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(repo: &Path, content: &str, message: &str) {
        std::fs::write(repo.join(AWKWARD), content).unwrap();
        git(repo, &["add", "--", AWKWARD]);
        git(repo, &["commit", "-q", "-m", message]);
    }

    /// A repo on `main` whose one file has an awkward name, and a branch
    /// `other` that changes it differently.
    fn diverged() -> (tempfile::TempDir, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("dir with space")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "T"]);
        git(&repo, &["config", "core.quotePath", "true"]);
        commit(&repo, "base\n", "base");
        git(&repo, &["checkout", "-q", "-b", "other"]);
        commit(&repo, "theirs\n", "theirs");
        git(&repo, &["checkout", "-q", "main"]);
        commit(&repo, "ours\n", "ours");
        (tmp, repo)
    }

    fn tip(repo: &Path, rev: &str) -> CommitId {
        CommitId::parse(&GitVcs::run_trimmed(["rev-parse", rev], repo).unwrap()).unwrap()
    }

    #[test]
    fn a_conflicted_awkward_path_is_named_as_it_is() {
        let (_tmp, repo) = diverged();
        let other = tip(&repo, "other");

        let unmerged = GitVcs::begin_checkout_merge(&repo, &other).unwrap();

        assert_eq!(unmerged, vec![AWKWARD.to_owned()]);
    }

    #[test]
    fn an_awkward_path_is_found_in_a_tree() {
        let (_tmp, repo) = diverged();
        let tree = crate::vcs::TreeId::from_object_name(
            GitVcs::run_trimmed(["rev-parse", "HEAD^{tree}"], &repo)
                .unwrap()
                .as_str(),
        );

        let content = GitVcs.file_in_tree(&repo, &tree, AWKWARD).unwrap();

        assert_eq!(content.as_deref(), Some("ours\n"));
    }

    #[test]
    fn an_awkward_path_drifted_under_a_worktree_is_named_and_refreshed_as_it_is() {
        let (tmp, repo) = diverged();
        let worktree = tmp.path().join("elsewhere");
        let base = tip(&repo, "main~1");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                worktree.to_str().unwrap(),
                base.as_str(),
            ],
        );
        git(&repo, &["update-ref", "refs/heads/feat", "main"]);

        let left = left_behind_at(&worktree).unwrap();
        assert_eq!(left.as_ref(), Some(&base));
        assert_eq!(
            path_standings(&worktree, left.as_ref()).unwrap(),
            vec![PathStanding {
                path: AWKWARD.to_owned(),
                moved: true,
                index: Holds::LeftTip,
                file: Holds::LeftTip,
            }]
        );
        refresh_paths_from_head(&worktree, &[AWKWARD.to_owned()], &[]).unwrap();
        assert_eq!(
            std::fs::read_to_string(worktree.join(AWKWARD))
                .unwrap()
                .replace("\r\n", "\n"),
            "ours\n"
        );
        assert!(path_standings(&worktree, left.as_ref()).unwrap().is_empty());
    }
}
