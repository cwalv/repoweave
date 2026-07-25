//! Git implementation of the [`Vcs`] trait.

use crate::manifest::Role;
use crate::vcs::{
    CommitSummary, ConflictOp, HeadAttachment, HeadObservation, LocalRefName, PreAbortRef,
    PublishRef, RawRefName, RawRevisionId, RefName, RemoteDefaultBranch, ResolvedRevisionId,
    UniqueDiff, Vcs, VcsError, VerifiedRestoreOutcome,
};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `GIT_*` environment variables that git itself sets for hooks (and that
/// other tooling sometimes sets) which silently misdirect any subprocess
/// `git` invocation if inherited.
const GIT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// Build a `git` command with all inherited `GIT_*` environment variables
/// stripped. rwv resolves its own paths via `WorkspaceContext`; inheriting
/// these vars from the surrounding process (a `pre-push` hook, another git
/// invocation, etc.) makes subprocess `git` operate on the wrong repo
/// regardless of the `current_dir` we set.
pub(crate) fn git_command() -> Command {
    let mut cmd = Command::new("git");
    for var in GIT_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd
}

// ---------------------------------------------------------------------------
// Replay-exclusion (rwv.lock) — merge-driver constants
// ---------------------------------------------------------------------------
//
// git's per-path merge driver mechanism has two halves:
//
//   1. `.gitattributes` line: `<path> merge=<driver-name>` — assigns a driver
//      by name to the path. This lives in the tree and travels with commits.
//   2. `merge.<driver-name>.driver` config: the shell command git runs to
//      resolve the merge. If the driver name is not defined in *any* config
//      git can see (system/global/local/inline), git falls back to a normal
//      3-way merge — so a `.gitattributes` assignment with no matching
//      `merge.<name>.driver` entry does NOTHING useful for `rwv.lock`.
//
// The driver name is a namespaced key — `rwv-ours` rather than the
// gitattributes(5) example name `ours`. Namespacing pairs the assignment
// and the definition: only rwv writes `merge=rwv-ours`, only rwv defines
// `merge.rwv-ours.driver`. Two accidental collisions the plain `ours` name
// created are closed by this choice:
//
//   - A third-party repo with an unrelated `<somepath> merge=ours` line
//     would previously get rwv's inline `merge.ours.driver=true` applied to
//     it during any rwv-driven rebase, silently keeping the target-side
//     version. `rwv-ours` never collides that way.
//   - A user's global `merge.ours.driver=true` would previously apply to
//     `rwv.lock merge=ours` when the operator ran bare `git rebase
//     --continue` (bug 1). Under `rwv-ours` an unrelated global cannot
//     activate the driver by accident.
//
// The constants are declared here so the inline `-c` flags in
// [`GitVcs::rebase`], the `.gitattributes` writer in
// [`GitVcs::set_replay_exclusion`], the readers in
// [`GitVcs::has_replay_exclusion`] and
// [`GitVcs::has_committed_replay_exclusion`], the invariant in
// `sync::verify_replay_exclusion_invariant`, and the doctor `--fix`
// migrator all reference a single source of truth for the name.

/// Merge-driver name assigned to `rwv.lock` in `.gitattributes` and defined
/// via `merge.<name>.driver` config.
pub const RWV_MERGE_DRIVER_NAME: &str = "rwv-ours";

/// `merge.<name>.driver` config key that defines the driver's shell command.
/// Setting this to `true` (the shell command `/bin/true`, i.e. no-op success)
/// tells git "keep the current (target-side) content unchanged when the
/// assigned path would otherwise 3-way merge".
pub const RWV_MERGE_DRIVER_CONFIG_KEY: &str = "merge.rwv-ours.driver";

/// `merge.<name>.name` config key — human-readable description git shows in
/// `git config --list` and diagnostic output. Not functional; paired with the
/// `.driver` entry so an operator inspecting config sees why the entry exists.
pub const RWV_MERGE_DRIVER_NAME_KEY: &str = "merge.rwv-ours.name";

/// Human-readable description written to [`RWV_MERGE_DRIVER_NAME_KEY`].
pub const RWV_MERGE_DRIVER_NAME_DESC: &str = "keep ours during replay (rwv replay-exclusion)";

/// Legacy driver name from before the rename. Read-only: the
/// migrator in [`GitVcs::set_replay_exclusion`] rewrites `merge=ours` lines
/// to the new name, and `sync::verify_replay_exclusion_invariant` produces
/// a targeted "run rwv doctor --fix" bail when it finds the legacy line
/// committed in `.gitattributes`.
pub const LEGACY_RWV_MERGE_DRIVER_NAME: &str = "ours";

/// Build the `.gitattributes` line that assigns the rwv driver to `path`.
///
/// Callers compare against this via `.trim() == needle` so leading/trailing
/// whitespace variations in a hand-edited `.gitattributes` don't defeat the
/// idempotence check.
pub fn rwv_replay_exclusion_needle(path_str: &str) -> String {
    format!("{path_str} merge={RWV_MERGE_DRIVER_NAME}")
}

/// Legacy needle (`<path> merge=ours`) recognised by the migrator so
/// `doctor --fix` and `set_replay_exclusion` can rewrite it in place.
pub fn legacy_rwv_replay_exclusion_needle(path_str: &str) -> String {
    format!("{path_str} merge={LEGACY_RWV_MERGE_DRIVER_NAME}")
}

/// Plant the durable repo-local `merge.rwv-ours.*` config that keeps the
/// exclusion working during bare `git rebase --continue` (the resume path
/// git itself advertises in conflict stderr). Idempotent — repeated writes
/// are no-ops.
///
/// rwv's own rebase invocations pass the definition inline as `-c` flags
/// (see [`GitVcs::rebase`]) so they don't depend on config state. But when
/// a rebase stops on a genuine non-lock conflict and the operator resumes
/// with plain `git rebase --continue`, that new git process has no inline
/// `-c` — without the durable config, every subsequent lock-only pick would
/// 3-way merge on `rwv.lock` and conflict.
///
/// Worktrees share `.git/config` with the canonical repo, so a single plant
/// covers every workweave checkout — `git config` resolves the right file
/// itself. Called from `sync::verify_replay_exclusion_invariant` (self-heals
/// before every rebase-strategy sync) and from `rwv doctor --fix`.
pub fn plant_rwv_merge_driver_config(repo: &Path) -> Result<(), VcsError> {
    let driver_status = git_command()
        .args(["config", RWV_MERGE_DRIVER_CONFIG_KEY, "true"])
        .current_dir(repo)
        .output()
        .map_err(|e| VcsError::Io {
            ctx: format!(
                "failed to spawn git config {RWV_MERGE_DRIVER_CONFIG_KEY} in {}",
                repo.display()
            ),
            source: e,
        })?;
    if !driver_status.status.success() {
        let stderr = String::from_utf8_lossy(&driver_status.stderr).into_owned();
        return Err(VcsError::CommandFailed {
            args: vec![
                "config".to_owned(),
                RWV_MERGE_DRIVER_CONFIG_KEY.to_owned(),
                "true".to_owned(),
            ],
            repo: repo.to_path_buf(),
            stderr,
        });
    }

    let name_status = git_command()
        .args([
            "config",
            RWV_MERGE_DRIVER_NAME_KEY,
            RWV_MERGE_DRIVER_NAME_DESC,
        ])
        .current_dir(repo)
        .output()
        .map_err(|e| VcsError::Io {
            ctx: format!(
                "failed to spawn git config {RWV_MERGE_DRIVER_NAME_KEY} in {}",
                repo.display()
            ),
            source: e,
        })?;
    if !name_status.status.success() {
        let stderr = String::from_utf8_lossy(&name_status.stderr).into_owned();
        return Err(VcsError::CommandFailed {
            args: vec![
                "config".to_owned(),
                RWV_MERGE_DRIVER_NAME_KEY.to_owned(),
                RWV_MERGE_DRIVER_NAME_DESC.to_owned(),
            ],
            repo: repo.to_path_buf(),
            stderr,
        });
    }
    Ok(())
}

/// Read `.gitattributes` from the committed tree at HEAD, if any.
///
/// Returns `Ok(None)` when the file isn't tracked at HEAD (fresh repo, or
/// `.gitattributes` was never added) — that's a definitive "no committed
/// content", not an error. Returns `Ok(Some(contents))` when the file is
/// tracked. Used by the sync precondition and doctor: they must consult
/// the committed form because a working-tree-only `.gitattributes` doesn't
/// survive a rebase and won't help subsequent operators.
pub fn read_committed_gitattributes(repo: &Path) -> Result<Option<String>, VcsError> {
    let output = git_command()
        .args(["show", "HEAD:.gitattributes"])
        .current_dir(repo)
        .output()
        .map_err(|e| VcsError::Io {
            ctx: format!(
                "failed to spawn git show HEAD:.gitattributes in {}",
                repo.display()
            ),
            source: e,
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// `true` when the committed `.gitattributes` at HEAD still carries the
/// **legacy** `<path> merge=ours` line (the rename replaced it
/// with `merge=rwv-ours`). Used by `sync::verify_replay_exclusion_invariant`
/// so its bail message can direct the operator at `rwv doctor --fix` for
/// migration rather than the generic "add the line" fix.
pub fn has_committed_legacy_replay_exclusion(repo: &Path, path: &Path) -> Result<bool, VcsError> {
    let path_str = path.to_str().ok_or_else(|| VcsError::Io {
        ctx: format!(
            "replay-exclusion path {} is not valid UTF-8",
            path.display()
        ),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "non-utf8 replay-exclusion path",
        ),
    })?;
    let legacy = legacy_rwv_replay_exclusion_needle(path_str);
    let content = match read_committed_gitattributes(repo)? {
        Some(c) => c,
        None => return Ok(false),
    };
    Ok(content.lines().any(|line| line.trim() == legacy))
}

/// `true` when the on-disk (working-tree) `.gitattributes` carries the
/// **legacy** `<path> merge=ours` line. Used by `rwv doctor` to detect
/// projects still on the legacy needle so `--fix` can migrate them.
pub fn has_working_tree_legacy_replay_exclusion(
    repo: &Path,
    path: &Path,
) -> Result<bool, VcsError> {
    let attrs_path = repo.join(".gitattributes");
    let path_str = path.to_str().ok_or_else(|| VcsError::Io {
        ctx: format!(
            "replay-exclusion path {} is not valid UTF-8",
            path.display()
        ),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "non-utf8 replay-exclusion path",
        ),
    })?;
    let legacy = legacy_rwv_replay_exclusion_needle(path_str);
    let contents = match std::fs::read_to_string(&attrs_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(VcsError::Io {
                ctx: format!("failed to read {}", attrs_path.display()),
                source: e,
            })
        }
    };
    Ok(contents.lines().any(|line| line.trim() == legacy))
}

/// Build the two `KEY=VALUE` argument strings that inline-define the
/// `rwv-ours` merge driver for a single git invocation.
///
/// Every git command that needs the driver defined (currently: [`GitVcs::rebase`]
/// and [`GitVcs::rebase_continue`]) prepends `-c <name>=<desc>` and
/// `-c <config-key>=true` from these strings. Extracted so the two callers
/// share a single spelling of the flag pair — the driver name / description /
/// value spread across [`RWV_MERGE_DRIVER_NAME_KEY`],
/// [`RWV_MERGE_DRIVER_NAME_DESC`], and [`RWV_MERGE_DRIVER_CONFIG_KEY`] must
/// stay in lockstep across every rwv-spawned rebase step, otherwise a
/// `rwv sync --continue` mid-rebase resume could reach a different resolution
/// than the fresh-start rebase phase that stopped there.
///
/// Returns `(name_arg, driver_arg)` where each is a single `KEY=VALUE`
/// string suitable as the argument after a `-c` flag.
pub(crate) fn rwv_ours_driver_flag_args() -> (String, String) {
    (
        format!("{RWV_MERGE_DRIVER_NAME_KEY}={RWV_MERGE_DRIVER_NAME_DESC}"),
        format!("{RWV_MERGE_DRIVER_CONFIG_KEY}=true"),
    )
}

/// `true` when `merge.rwv-ours.driver` is set (to anything) in any config
/// scope git can see for `repo`. Used by `rwv doctor` to detect projects
/// that haven't had the durable plant run yet.
pub fn has_rwv_merge_driver_config(repo: &Path) -> Result<bool, VcsError> {
    let output = git_command()
        .args(["config", "--get", RWV_MERGE_DRIVER_CONFIG_KEY])
        .current_dir(repo)
        .output()
        .map_err(|e| VcsError::Io {
            ctx: format!(
                "failed to spawn git config --get {RWV_MERGE_DRIVER_CONFIG_KEY} in {}",
                repo.display()
            ),
            source: e,
        })?;
    // `git config --get` exits 0 when the key is set, 1 when unset. Other
    // non-zero exits (invalid key syntax, config file unreadable) are rare
    // enough that we surface them as errors rather than silently treating
    // as "unset".
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(VcsError::CommandFailed {
        args: vec![
            "config".to_owned(),
            "--get".to_owned(),
            RWV_MERGE_DRIVER_CONFIG_KEY.to_owned(),
        ],
        repo: repo.to_path_buf(),
        stderr,
    })
}

/// Git-based version control operations.
pub struct GitVcs;

impl GitVcs {
    /// Run a git command in `dir` and return trimmed stdout on success.
    ///
    /// Maps process I/O failure to [`VcsError::Io`] and non-zero exit to
    /// [`VcsError::CommandFailed`] with the args and stderr captured. Callers
    /// that can detect more specific failures (revision not found, branch
    /// already exists, ...) should match on the resulting `CommandFailed`
    /// stderr and remap.
    fn run(args: &[&str], dir: &Path) -> Result<String, VcsError> {
        let output = git_command()
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(VcsError::CommandFailed {
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                repo: dir.to_path_buf(),
                stderr,
            });
        }

        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_string())
            .map_err(|_| VcsError::CommandFailed {
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                repo: dir.to_path_buf(),
                stderr: "git output not valid UTF-8".to_string(),
            })
    }
}

impl GitVcs {
    /// Check if `ancestor` is a strict ancestor of `descendant` in `repo`.
    ///
    /// Uses `git merge-base --is-ancestor`. Returns `Ok(false)` when the
    /// objects are the same (equal, not strictly ancestral).
    pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
        if ancestor == descendant {
            return false;
        }
        git_command()
            .args(["merge-base", "--is-ancestor", ancestor, descendant])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Resolve `rev` to its canonical 40-hex SHA in `repo`.
    ///
    /// Thin wrapper over `git rev-parse --verify <rev>^{commit}`. Private
    /// helper for the [`Vcs::unique_commits`] / [`Vcs::unique_diff`] impls,
    /// which resolve the workweave's HEAD before computing the parent-relative
    /// range.
    fn rev_parse(repo: &Path, rev: &str) -> Result<String, VcsError> {
        let deref = format!("{rev}^{{commit}}");
        Self::run(&["rev-parse", "--verify", &deref], repo)
    }

    /// Compute the merge-base (common ancestor) of `a` and `b` in `repo`.
    ///
    /// Returns the common-ancestor SHA. Private helper for [`Vcs::unique_diff`],
    /// which anchors the diff range at `git merge-base <parent-tip> HEAD`
    /// rather than the parent tip directly — diffing against a parent tip that
    /// advanced after the fork shows phantom reversals of the work the parent
    /// gained in the meantime.
    fn merge_base(repo: &Path, a: &str, b: &str) -> Result<String, VcsError> {
        Self::run(&["merge-base", a, b], repo)
    }

    /// List the commits reachable from `to` but not from `from` in `repo`,
    /// newest first, as [`CommitSummary`] records.
    ///
    /// This is `git log <from>..<to>` semantics: with `from` = the parent tip
    /// and `to` = HEAD, the result is exactly the workweave's UNIQUE commits,
    /// and it stays correct when the parent advanced since the fork (the
    /// range excludes commits the parent already has). An empty vec means no
    /// unique commits. Private helper for [`Vcs::unique_commits`].
    fn commits_in_range(repo: &Path, from: &str, to: &str) -> Result<Vec<CommitSummary>, VcsError> {
        // `%H` full SHA, `%h` short SHA, `%s` subject — NUL-delimited fields,
        // newline-delimited records, so subjects with spaces/tabs survive.
        let range = format!("{from}..{to}");
        let fmt = "--pretty=format:%H%x00%h%x00%s";
        let out = Self::run(&["log", fmt, &range], repo)?;
        let entries = out
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\0');
                let id = parts.next()?.to_string();
                let short = parts.next()?.to_string();
                let subject = parts.next().unwrap_or("").to_string();
                Some(CommitSummary { id, short, subject })
            })
            .collect();
        Ok(entries)
    }

    /// Produce the unified diff of `from..to` in `repo`.
    ///
    /// Private helper for [`Vcs::unique_diff`], which passes `from` = `git
    /// merge-base <parent-tip> HEAD` and `to` = HEAD so the output is the
    /// workweave's unique work with no phantom reversals.
    fn diff_range(repo: &Path, from: &str, to: &str) -> Result<String, VcsError> {
        let range = format!("{from}..{to}");
        Self::run(&["diff", &range], repo)
    }

    /// The directory to run ref-level git commands in for a canonical
    /// store path.
    ///
    /// A receipt is keyed by canonical store, and `resolve_canonical_store`
    /// reports the store itself (`<clone>/.git`), not the working
    /// directory. git will accept a `.git` directory as its cwd, but the
    /// behaviour differs subtly between porcelain commands, so normalise to
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
        match full.strip_prefix("refs/heads/") {
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
    /// detached-MOVE precondition (§3.6) exists to see.
    pub fn mid_op_state(repo: &Path) -> Option<String> {
        let git_dir = match Self::run(&["rev-parse", "--git-dir"], repo) {
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
            return Some("mid-rebase".to_owned());
        }
        if git_dir.join("MERGE_HEAD").exists() {
            return Some("mid-merge".to_owned());
        }
        if git_dir.join("CHERRY_PICK_HEAD").exists() {
            return Some("mid-cherry-pick".to_owned());
        }
        if git_dir.join("BISECT_LOG").exists() {
            return Some("mid-bisect".to_owned());
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
    /// `"commit abc1234 (lock: refresh — post-OOB drift in gc-formulas)"`
    fn rebase_stopped_commit_detail_impl(repo: &Path) -> String {
        let fallback = "see in-flight rebase state for conflicting paths".to_owned();

        // Resolve the .git directory so we can locate rebase-merge/stopped-sha.
        let git_dir = match Self::run(&["rev-parse", "--git-dir"], repo) {
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

        // Read the full SHA of the stopped commit.
        let stopped_sha_path = git_dir.join("rebase-merge").join("stopped-sha");
        let full_sha = match std::fs::read_to_string(&stopped_sha_path) {
            Ok(s) => s.trim().to_owned(),
            Err(_) => return fallback,
        };
        if full_sha.is_empty() {
            return fallback;
        }

        // Shorten the SHA for display.
        let short_sha = Self::run(&["rev-parse", "--short", &full_sha], repo)
            .unwrap_or_else(|_| full_sha.chars().take(7).collect());

        // Fetch the commit subject line.
        let subject = match Self::run(&["log", "-1", "--format=%s", &full_sha], repo) {
            Ok(s) => s.trim().to_owned(),
            Err(_) => return format!("commit {short_sha}"),
        };
        if subject.is_empty() {
            return format!("commit {short_sha}");
        }

        format!("commit {short_sha} ({subject})")
    }

    /// Return up to `cap` one-line commit summaries for the range `from..to`
    /// plus the total commit count in that range.
    ///
    /// Uses `git log --oneline -<cap+1> <from>..<to>` to fetch at most
    /// `cap + 1` lines, then returns `(lines[..cap], total)` where `total`
    /// is the actual `git rev-list --count` so the caller can display "and N
    /// more" when `total > cap`.
    ///
    /// Returns `(vec![], 0)` on any error so callers can degrade gracefully
    /// when the range is unresolvable (e.g. the object is unreachable in a
    /// shallow clone or the SHA is malformed).
    fn log_oneline_range_impl(
        repo: &Path,
        from: &str,
        to: &str,
        cap: usize,
    ) -> (Vec<String>, usize) {
        // Fetch up to cap+1 lines so we can detect truncation without a
        // separate rev-list call in the common case where total <= cap.
        let limit_arg = format!("-{}", cap + 1);
        let range = format!("{from}..{to}");
        let log_out = match Self::run(&["log", "--oneline", &limit_arg, &range], repo) {
            Ok(s) => s,
            Err(_) => return (vec![], 0),
        };
        let lines: Vec<String> = log_out
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();

        if lines.len() <= cap {
            // All commits fit — total equals what we received.
            let total = lines.len();
            (lines, total)
        } else {
            // More than cap commits exist; count the full range.
            let count_out = Self::run(&["rev-list", "--count", &range], repo).unwrap_or_default();
            let total = count_out.trim().parse::<usize>().unwrap_or(lines.len());
            (lines[..cap].to_vec(), total)
        }
    }

    /// Return `(ahead, behind)` commit counts for `savepoint..tip` and
    /// `tip..savepoint` respectively. Used to determine whether tip is
    /// strictly ahead of savepoint (behind == 0, ahead > 0) or diverged
    /// (both > 0).
    ///
    /// Returns `(0, 0)` on any git error.
    fn ahead_behind_impl(repo: &Path, savepoint: &str, tip: &str) -> (usize, usize) {
        let ahead = Self::run(
            &["rev-list", "--count", &format!("{savepoint}..{tip}")],
            repo,
        )
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
        let behind = Self::run(
            &["rev-list", "--count", &format!("{tip}..{savepoint}")],
            repo,
        )
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
        (ahead, behind)
    }
}

impl GitVcs {
    /// Initialize a new git repo at `dest`.
    pub fn init_repo(&self, dest: &Path) -> Result<(), VcsError> {
        std::fs::create_dir_all(dest).map_err(|e| VcsError::Io {
            ctx: format!("failed to create directory {}", dest.display()),
            source: e,
        })?;
        Self::run(&["init", "--initial-branch=main"], dest)?;
        Ok(())
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
/// `rwv sync --continue` / `rwv sync-to --continue` that covers all
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
            // `rwv sync-to --continue` — the VCS impl must not spell rwv
            // vocabulary.
            "  # edit conflicted files\n  git add <files>".to_string()
        }
        ConflictOp::Merge => {
            "  # edit conflicted files\n  git add <files>\n  git merge --continue".to_string()
        }
        ConflictOp::CherryPick => {
            "  # edit conflicted files\n  git add <files>\n  git cherry-pick --continue".to_string()
        }
    }
}

/// True when stderr signals "revision unknown / no such object".
fn is_revision_not_found(stderr: &str) -> bool {
    stderr.contains("unknown revision")
        || stderr.contains("not a valid object name")
        || stderr.contains("ambiguous argument")
        || stderr.contains("Needed a single revision")
}

/// True when stderr signals "branch already exists / worktree already exists".
fn is_already_exists(stderr: &str) -> bool {
    stderr.contains("already exists") || stderr.contains("already a worktree")
}

/// True for transient/internal tags that must not be chosen as a lock's
/// symbolic name. Mirrors the ref-spaces rwv uses for its own bookkeeping —
/// `savepoint/*` (operator/tool savepoints), `rwv/pre-op/*` (sync abort
/// recovery refs under `refs/rwv/pre-op/*` when surfaced as tag names),
/// and `rwv/pre-abort/*` (pre-abort recovery refs under
/// `refs/rwv/pre-abort/*`).
fn is_transient_tag(tag: &str) -> bool {
    tag.starts_with("savepoint/")
        || tag.starts_with("rwv/pre-op/")
        || tag.starts_with("refs/rwv/pre-op/")
        || tag.starts_with("rwv/pre-abort/")
        || tag.starts_with("refs/rwv/pre-abort/")
        || tag.starts_with("rwv-savepoint/")
}

/// True for release-shape tags (e.g., `v1.2.3`, `v0.3.4-rc1`). Used as a
/// tiebreaker when multiple non-transient tags point at HEAD so a release
/// tag wins over an arbitrary lightweight tag.
///
/// The rule itself lives at the seam
/// ([`crate::vcs::is_release_shape_name`]) because `TrackingRef::parse`
/// asks the same question for the opposite purpose — rejecting a
/// `version:` that names a release rather than a channel. One definition,
/// two callers.
fn is_release_shape_tag(tag: &str) -> bool {
    crate::vcs::is_release_shape_name(tag)
}

impl Vcs for GitVcs {
    fn name(&self) -> &str {
        "git"
    }

    fn clone_repo(&self, url: &str, dest: &Path) -> Result<(), VcsError> {
        let dest_str = dest.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!("destination path {} is not valid UTF-8", dest.display()),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "non-utf8 destination path",
            ),
        })?;
        Self::run(&["clone", url, dest_str], Path::new("."))?;
        Ok(())
    }

    fn clone_repo_with_remote_name(
        &self,
        url: &str,
        dest: &Path,
        remote_name: &str,
    ) -> Result<(), VcsError> {
        let dest_str = dest.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!("destination path {} is not valid UTF-8", dest.display()),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "non-utf8 destination path",
            ),
        })?;
        Self::run(
            &["clone", "--origin", remote_name, url, dest_str],
            Path::new("."),
        )?;
        Ok(())
    }

    fn clone_with_role(&self, url: &str, dest: &Path, role: Role) -> Result<(), VcsError> {
        let _ = role; // role label kept for signal value; all clones use `origin`
        self.clone_repo_with_remote_name(url, dest, "origin")
    }

    fn resolve_branch_on_remote(
        &self,
        repo: &Path,
        role: Role,
        branch: &RefName,
    ) -> Result<ResolvedRevisionId, VcsError> {
        let _ = role; // all remotes use `origin`
        let qualified = format!("origin/{}", branch.as_str());
        self.resolve_revision(repo, &qualified)
    }

    fn push_with_role(&self, repo: &Path, role: Role, force: bool) -> Result<(), VcsError> {
        // Resolve the currently-checked-out branch. A detached HEAD has no
        // branch to push as a ref update; surface a `CommandFailed` with a
        // stderr that names the condition so callers without an out-of-band
        // pre-check still see a clear message.
        let branch = match self.current_ref(repo)? {
            Some(b) => b,
            None => {
                return Err(VcsError::CommandFailed {
                    args: vec!["push".to_owned()],
                    repo: repo.to_path_buf(),
                    stderr: "cannot push: HEAD is detached (no branch)".to_owned(),
                });
            }
        };
        let _ = role; // all remotes use `origin`
        let mut args: Vec<&str> = vec!["push"];
        if force {
            args.push("--force");
        }
        args.push("origin");
        args.push(branch.as_str());
        Self::run(&args, repo)?;
        Ok(())
    }

    fn head_revision(&self, repo: &Path) -> Result<ResolvedRevisionId, VcsError> {
        match Self::run(&["rev-parse", "HEAD"], repo) {
            Err(VcsError::CommandFailed { stderr, .. })
                if stderr.contains("ambiguous argument") =>
            {
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
                    args: vec!["rev-parse".to_owned(), "HEAD".to_owned()],
                    repo: repo.to_path_buf(),
                    stderr: format!(
                        "unborn HEAD (no commits yet, on branch '{branch_hint}'): \
                         make an initial commit, then re-run rwv lock"
                    ),
                })
            }
            Err(e) => Err(e),
            Ok(sha) => {
                // If a tag points at HEAD, preserve it as the display form so callers
                // get human-readable round-trips (e.g., `v0.3.4`) without an extra
                // resolve step.
                let display = self.tag_at_head(repo)?.map(|t| t.as_str().to_string());
                Ok(ResolvedRevisionId::from_canonical(sha, display))
            }
        }
    }

    fn resolve_revision(&self, repo: &Path, rev: &str) -> Result<ResolvedRevisionId, VcsError> {
        let deref = format!("{rev}^{{commit}}");
        match Self::run(&["rev-parse", "--verify", &deref], repo) {
            Ok(canonical) => {
                let display = if rev == canonical {
                    None
                } else {
                    Some(rev.to_string())
                };
                Ok(ResolvedRevisionId::from_canonical(canonical, display))
            }
            Err(VcsError::CommandFailed { stderr, .. }) if is_revision_not_found(&stderr) => {
                Err(VcsError::RevisionNotFound {
                    repo: repo.to_path_buf(),
                    rev: rev.to_string(),
                })
            }
            Err(e) => Err(e),
        }
    }

    fn current_ref(&self, repo: &Path) -> Result<Option<RefName>, VcsError> {
        match Self::run(&["symbolic-ref", "--short", "HEAD"], repo) {
            Ok(name) => Ok(Some(RefName::new(name))),
            Err(_) => Ok(None), // detached HEAD
        }
    }

    fn create_worktree(
        &self,
        repo: &Path,
        dest: &Path,
        branch_name: &RefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        let dest_str = dest.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!("worktree path {} is not valid UTF-8", dest.display()),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-utf8 worktree path"),
        })?;
        let start = start_point.as_str();
        let branch = branch_name.as_str();

        // First try creating a new branch with -b.
        let result = Self::run(&["worktree", "add", "-b", branch, dest_str, start], repo);

        if let Err(e) = result {
            // If the branch already exists, try using it as-is (no -b).
            // This handles the case where a previous delete didn't clean up branches.
            let already = matches!(
                &e,
                VcsError::CommandFailed { stderr, .. } if is_already_exists(stderr)
            );
            if already {
                // Delete the stale branch first, then retry with -b.
                // If delete fails, fall back to using the existing branch directly.
                let deleted = Self::run(&["branch", "-D", branch], repo).is_ok();
                if deleted {
                    Self::run(&["worktree", "add", "-b", branch, dest_str, start], repo)?;
                } else {
                    Self::run(&["worktree", "add", dest_str, branch], repo)?;
                }
            } else {
                return Err(e);
            }
        }

        Ok(())
    }

    fn remove_worktree(&self, repo: &Path, worktree_path: &Path) -> Result<(), VcsError> {
        let wt_str = worktree_path.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!(
                "worktree path {} is not valid UTF-8",
                worktree_path.display()
            ),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-utf8 worktree path"),
        })?;
        Self::run(&["worktree", "remove", "--force", wt_str], repo)?;
        Ok(())
    }

    fn is_repo(&self, path: &Path) -> bool {
        Self::run(&["rev-parse", "--git-dir"], path).is_ok()
    }

    fn list_worktrees(&self, repo: &Path) -> Result<Vec<PathBuf>, VcsError> {
        let output = Self::run(&["worktree", "list", "--porcelain"], repo)?;
        let paths = output
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .collect();
        Ok(paths)
    }

    fn has_uncommitted_changes(&self, repo: &Path) -> Result<bool, VcsError> {
        // `git status --porcelain` prints one line per dirty entry;
        // empty output means the tree is clean.
        let output = Self::run(&["status", "--porcelain"], repo)?;
        Ok(!output.is_empty())
    }

    fn tag_at_head(&self, repo: &Path) -> Result<Option<RefName>, VcsError> {
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
        let output = Self::run(&["tag", "--points-at", "HEAD"], repo)?;
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
        Ok(Some(RefName::new(chosen)))
    }

    fn checkout(&self, repo: &Path, revision: &ResolvedRevisionId) -> Result<(), VcsError> {
        Self::run(&["checkout", revision.as_str()], repo)?;
        Ok(())
    }

    fn delete_branch(&self, repo: &Path, branch: &RefName) -> Result<(), VcsError> {
        Self::run(&["branch", "-D", branch.as_str()], repo)?;
        Ok(())
    }

    fn worktree_prune(&self, repo: &Path) -> Result<(), VcsError> {
        Self::run(&["worktree", "prune"], repo)?;
        Ok(())
    }

    fn list_branches_with_prefix(
        &self,
        repo: &Path,
        prefix: &RefName,
    ) -> Result<Vec<RefName>, VcsError> {
        // `git branch --list 'prefix/*'` lists all local branches under the prefix.
        let pattern = format!("{}/*", prefix.as_str());
        let output = Self::run(&["branch", "--list", &pattern], repo)?;
        let branches = output
            .lines()
            .map(|line| {
                // Lines from `git branch` are prefixed with "* " (current) or "  ".
                line.trim_start_matches('*').trim().to_string()
            })
            .filter(|s| !s.is_empty())
            .map(RefName::new)
            .collect();
        Ok(branches)
    }

    fn default_branch(&self, repo: &Path) -> Result<RefName, VcsError> {
        const FALLBACK: &str = "main";

        // All rwv clones use `origin`. Strip the prefix to recover the bare
        // branch name; fall back to "main" if the symref isn't set yet.
        let sym = "refs/remotes/origin/HEAD";
        if let Ok(sym_ref) = Self::run(&["symbolic-ref", sym], repo) {
            let branch = sym_ref
                .strip_prefix("refs/remotes/origin/")
                .unwrap_or(FALLBACK)
                .to_string();
            return Ok(RefName::new(branch));
        }
        Ok(RefName::new(FALLBACK))
    }

    fn conflict_resolution_hint(&self, op: ConflictOp) -> String {
        git_conflict_resolution_hint(op)
    }

    fn rebase(
        &self,
        repo: &Path,
        onto: &ResolvedRevisionId,
        upstream: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        // Wire up the `rwv-ours` merge driver inline (no persistent
        // `.git/config` change) so the `merge=rwv-ours` lines written by
        // [`set_replay_exclusion`] resolve to "keep the rebase-target's
        // version" — `driver = true` is the shell command `true`, which
        // succeeds without modifying the merged file. Doing this per
        // invocation (rather than only via durable config) means the
        // driver is available even when someone runs rwv against a repo
        // whose local config hasn't been planted yet (fresh clone before
        // doctor --fix, or before verify_replay_exclusion_invariant's
        // self-heal has run for the first time). The durable plant in
        // `plant_rwv_merge_driver_config` is what keeps the driver defined
        // across a bare `git rebase --continue` — see that function for
        // the rationale.
        //
        // [`set_replay_exclusion`]: Vcs::set_replay_exclusion
        // `git rebase --onto <onto> <upstream>` replays commits in
        // <upstream>..HEAD onto <onto>. On conflict, git leaves the repo
        // mid-rebase (rebase-merge/ + conflict markers in WT). We detect
        // that state and surface VcsError::RebaseConflict so the caller can
        // pair with conflict_resolution_hint(ConflictOp::Rebase).
        // `--empty=drop`: drop commits that become empty after rebase. This
        // is what makes lock-only commits silently disappear when the
        // `merge=rwv-ours` driver on rwv.lock (configured via
        // [`set_replay_exclusion`]) leaves nothing for the commit to record.
        //
        // `--no-keep-empty`: also drop commits that were originally empty
        // (e.g. `git commit --allow-empty`). The old custom cherry-pick loop
        // skipped these via empty-patch detection; preserve that behaviour
        // so a relock-noise commit doesn't survive a rebase.
        //
        // `--force-rebase`: force a replay even when `upstream` is already
        // an ancestor of HEAD. Without it, git short-circuits to "up to
        // date" — and lock-only commits that should be dropped survive.
        // sync's invariant is "the project repo's history past the source
        // tip is a replayable subset"; forcing the replay makes that
        // invariant true after every rebase regardless of which side moved.
        //
        // [`set_replay_exclusion`]: Vcs::set_replay_exclusion
        let (driver_name_flag, driver_flag) = rwv_ours_driver_flag_args();
        let output = git_command()
            .args([
                "-c",
                driver_name_flag.as_str(),
                "-c",
                driver_flag.as_str(),
                "rebase",
                "--force-rebase",
                "--no-keep-empty",
                "--empty=drop",
                "--onto",
                onto.as_str(),
                upstream.as_str(),
            ])
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!(
                    "failed to spawn git rebase --onto {} {}",
                    onto.as_str(),
                    upstream.as_str()
                ),
                source: e,
            })?;

        if output.status.success() {
            return Ok(());
        }

        // Non-zero exit. If the repo is in mid-rebase, this is a conflict;
        // otherwise it's some other rebase error (bad refs, etc.).
        if matches!(Self::mid_op_state(repo).as_deref(), Some("mid-rebase")) {
            return Err(VcsError::RebaseConflict {
                repo: repo.to_path_buf(),
                op: ConflictOp::Rebase,
            });
        }

        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(VcsError::CommandFailed {
            args: vec![
                "rebase".to_owned(),
                "--force-rebase".to_owned(),
                "--no-keep-empty".to_owned(),
                "--empty=drop".to_owned(),
                "--onto".to_owned(),
                onto.as_str().to_owned(),
                upstream.as_str().to_owned(),
            ],
            repo: repo.to_path_buf(),
            stderr,
        })
    }

    fn rebase_continue(&self, repo: &Path) -> Result<(), VcsError> {
        // Caller contract: `repo` must be mid-rebase. Enforce it here — the
        // call site (sync's replay re-entry) already inspects `mid_op` to
        // route between `rebase` and this method, so reaching this function
        // on a clean repo is a bug, not an in-band condition. Silent no-op
        // would hide it; instead surface a `CommandFailed` naming the wrong
        // state, matching how `rebase` itself falls through to
        // `CommandFailed` when a non-conflict rebase failure isn't
        // classifiable further.
        if !matches!(Self::mid_op_state(repo).as_deref(), Some("mid-rebase")) {
            return Err(VcsError::CommandFailed {
                args: vec!["rebase".to_owned(), "--continue".to_owned()],
                repo: repo.to_path_buf(),
                stderr: format!(
                    "rebase_continue called on {} which is not mid-rebase",
                    repo.display()
                ),
            });
        }

        // Re-supply the `rwv-ours` merge-driver flags inline. The durable
        // config plant is what makes bare `git rebase --continue` safe for
        // the operator, but rwv-driven rebase steps must not depend on
        // config state (they can run against a fresh clone that has not had
        // the plant self-heal yet, so keep the driver definition inline for
        // every rwv-spawned git subprocess). Same flag pair as
        // [`GitVcs::rebase`] via `rwv_ours_driver_flag_args`.
        let (driver_name_flag, driver_flag) = rwv_ours_driver_flag_args();

        // `git rebase --continue` invokes `$EDITOR` on the stopped commit's
        // message before recording it. rwv is a non-interactive tool driven
        // from CI and shell scripts, so an editor spawn here would hang the
        // process. Pin both `GIT_EDITOR` and `GIT_SEQUENCE_EDITOR` to the
        // `true` command — the same convention the test harness uses (see
        // `tests/common/mod.rs`). Env is scoped to this single subprocess;
        // the operator's own `git rebase --continue` outside rwv is
        // unaffected.
        let output = git_command()
            .args([
                "-c",
                driver_name_flag.as_str(),
                "-c",
                driver_flag.as_str(),
                "rebase",
                "--continue",
            ])
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

        // Non-zero exit. If the repo is STILL mid-rebase, git either
        // stopped on a further genuine conflict OR the operator's
        // resolution left conflict markers unstaged ("needs merge" / "must
        // edit all merge conflicts"). Both surface as the same operator
        // signal: resolve → stage → rerun `--continue`. If the repo is no
        // longer mid-rebase, this is some other rebase error we don't have
        // a specific class for; fall through to `CommandFailed`.
        if matches!(Self::mid_op_state(repo).as_deref(), Some("mid-rebase")) {
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

    fn set_replay_exclusion(&self, repo: &Path, path: &Path) -> Result<(), VcsError> {
        let attrs_path = repo.join(".gitattributes");
        let path_str = path.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!(
                "replay-exclusion path {} is not valid UTF-8",
                path.display()
            ),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "non-utf8 replay-exclusion path",
            ),
        })?;
        let needle = rwv_replay_exclusion_needle(path_str);
        let legacy_needle = legacy_rwv_replay_exclusion_needle(path_str);

        let existing = match std::fs::read_to_string(&attrs_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(VcsError::Io {
                    ctx: format!("failed to read {}", attrs_path.display()),
                    source: e,
                })
            }
        };

        // Migration path: if the file still carries the legacy
        // `<path> merge=ours` line, rewrite it in place to the namespaced
        // `<path> merge=rwv-ours` name. Rewrite — don't append alongside —
        // because two conflicting `merge=` assignments on the same path in
        // the same .gitattributes file are ill-defined (last-wins, in
        // reading order), and leaving the old line in place would activate
        // any user's global `merge.ours.driver` on rwv.lock during bare
        // `git rebase --continue` (the exact hazard the rename closes).
        // Detection is trim-only so a hand-edited file with a trailing
        // space or CRLF still migrates.
        let has_legacy = existing.lines().any(|line| line.trim() == legacy_needle);
        let has_new = existing.lines().any(|line| line.trim() == needle);
        if has_new && !has_legacy {
            return Ok(());
        }

        let (mut next, migrated) = if has_legacy {
            let mut out = String::with_capacity(existing.len());
            let mut wrote_new = has_new;
            for line in existing.split_inclusive('\n') {
                // `split_inclusive('\n')` preserves the trailing newline
                // (and yields a final line without one when the file
                // doesn't end in `\n`). Compare the trimmed form so a
                // trailing `\r` (CRLF) or stray whitespace on the legacy
                // line still matches.
                let bare = line.trim_end_matches(['\n', '\r']);
                if bare.trim() == legacy_needle {
                    if !wrote_new {
                        // Preserve the caller's ending style: if the
                        // legacy line had a trailing newline, so does the
                        // replacement.
                        out.push_str(&needle);
                        if line.ends_with('\n') {
                            out.push('\n');
                        }
                        wrote_new = true;
                    }
                    // Drop this line (either replaced above or dropped as
                    // a duplicate of the already-present new needle).
                } else {
                    out.push_str(line);
                }
            }
            (out, true)
        } else {
            (existing, false)
        };

        // Append the new needle if it wasn't planted via migration and
        // wasn't already present. Ensure exactly one trailing newline
        // before the new line so concatenation is clean whether the file
        // ended with a newline or not.
        if !migrated && !has_new {
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            next.push_str(&needle);
            next.push('\n');
        }

        std::fs::write(&attrs_path, next).map_err(|e| VcsError::Io {
            ctx: format!("failed to write {}", attrs_path.display()),
            source: e,
        })?;
        Ok(())
    }

    fn has_replay_exclusion(&self, repo: &Path, path: &Path) -> Result<bool, VcsError> {
        let attrs_path = repo.join(".gitattributes");
        let path_str = path.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!(
                "replay-exclusion path {} is not valid UTF-8",
                path.display()
            ),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "non-utf8 replay-exclusion path",
            ),
        })?;
        let needle = rwv_replay_exclusion_needle(path_str);

        let contents = match std::fs::read_to_string(&attrs_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                return Err(VcsError::Io {
                    ctx: format!("failed to read {}", attrs_path.display()),
                    source: e,
                })
            }
        };

        Ok(contents.lines().any(|line| line.trim() == needle))
    }

    fn has_committed_replay_exclusion(&self, repo: &Path, path: &Path) -> Result<bool, VcsError> {
        let path_str = path.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!(
                "replay-exclusion path {} is not valid UTF-8",
                path.display()
            ),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "non-utf8 replay-exclusion path",
            ),
        })?;
        let needle = rwv_replay_exclusion_needle(path_str);
        let content = match read_committed_gitattributes(repo)? {
            Some(c) => c,
            None => return Ok(false),
        };
        Ok(content.lines().any(|line| line.trim() == needle))
    }

    fn advance_if_fast_forward(
        &self,
        repo: &Path,
        to: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        Self::run(&["merge", "--ff-only", to.as_str()], repo)?;
        Ok(())
    }

    fn hard_reset(&self, repo: &Path, to: &ResolvedRevisionId) -> Result<(), VcsError> {
        Self::run(&["reset", "--hard", to.as_str()], repo)?;
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
                ancestor.as_str(),
                descendant.as_str(),
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
        let range = format!("{}..{}", from.as_str(), to.as_str());
        let out = Self::run(&["rev-list", "--count", &range], repo)?;
        Ok(out.trim().parse::<usize>().unwrap_or(0))
    }

    fn create_savepoint(&self, repo: &Path, op_id: &str) -> Result<ResolvedRevisionId, VcsError> {
        let head = self.head_revision(repo)?;
        let ref_name = savepoint_ref(op_id);
        Self::run(&["update-ref", &ref_name, head.as_str()], repo)?;
        Ok(head)
    }

    fn resolve_savepoint(&self, repo: &Path, op_id: &str) -> Option<ResolvedRevisionId> {
        let ref_name = savepoint_ref(op_id);
        let out = Self::run(&["rev-parse", &ref_name], repo).ok()?;
        ResolvedRevisionId::from_rev_parse_output(&out)
    }

    fn restore_savepoint(&self, repo: &Path, op_id: &str) -> Result<bool, VcsError> {
        match self.resolve_savepoint(repo, op_id) {
            Some(sha) => {
                Self::run(&["reset", "--hard", sha.as_str()], repo)?;
                self.drop_savepoint(repo, op_id);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn drop_savepoint(&self, repo: &Path, op_id: &str) {
        let ref_name = savepoint_ref(op_id);
        let _ = Self::run(&["update-ref", "-d", &ref_name], repo);
    }

    fn create_pre_abort_ref(&self, repo: &Path, op_id: &str) -> Result<PreAbortRef, VcsError> {
        // First write wins: a re-run of abort for the same op (e.g. after a
        // foreign-tip refusal was reconciled) must not overwrite the original
        // capture — by then the operator may have moved the branch, leaving
        // this ref as the only remaining reference to the pre-abort tip.
        if let Some(existing) = self.resolve_pre_abort_ref(repo, op_id) {
            return Ok(existing);
        }
        let head = self.head_revision(repo)?;
        let label = pre_abort_ref(op_id);
        Self::run(&["update-ref", &label, head.as_str()], repo)?;
        Ok(PreAbortRef {
            label,
            revision: head,
        })
    }

    fn resolve_pre_abort_ref(&self, repo: &Path, op_id: &str) -> Option<PreAbortRef> {
        let label = pre_abort_ref(op_id);
        let canonical = Self::run(&["rev-parse", &label], repo).ok()?;
        Some(PreAbortRef {
            revision: ResolvedRevisionId::from_rev_parse_output(&canonical)?,
            label,
        })
    }

    fn verified_restore_savepoint(
        &self,
        repo: &Path,
        op_id: &str,
        recorded_intent_tip: Option<&str>,
        recorded_converged_tip: Option<&str>,
    ) -> Result<VerifiedRestoreOutcome, VcsError> {
        // Resolve the savepoint first: no savepoint → nothing to do.
        let savepoint = match self.resolve_savepoint(repo, op_id) {
            Some(sp) => sp,
            None => return Ok(VerifiedRestoreOutcome::NoSavepoint),
        };

        // VCS-native mid-op state is wreckage attributable to the op
        // (replay's strategy ops move tips through these states). Cancel
        // the mid-op first, then restore.
        if self.mid_op(repo).is_some() {
            self.cancel_in_flight_op(repo);
            return self.reset_and_drop_savepoint(
                repo,
                op_id,
                &savepoint,
                VerifiedRestoreOutcome::RestoredFromMidOp,
            );
        }

        // Classify the current tip against the enumerable attributable set.
        let head = self.head_revision(repo)?;
        let head_sha = head.as_str();

        if head_sha == savepoint.as_str() {
            // Untouched — restore is a no-op. Still drop the savepoint so
            // abort's cleanup leaves no stale refs (matches restore_savepoint's
            // post-condition; the pre-abort ref preserves the tip regardless).
            self.drop_savepoint(repo, op_id);
            return Ok(VerifiedRestoreOutcome::Untouched);
        }

        // Intent tip: the op advanced this repo during replay (before relock).
        // Exact-match only — no heuristic (§6 rules out any descendant check).
        if let Some(intent) = recorded_intent_tip {
            if head_sha == intent {
                return self.reset_and_drop_savepoint(
                    repo,
                    op_id,
                    &savepoint,
                    VerifiedRestoreOutcome::RestoredFromIntent,
                );
            }
        }

        if let Some(converged) = recorded_converged_tip {
            if head_sha == converged {
                return self.reset_and_drop_savepoint(
                    repo,
                    op_id,
                    &savepoint,
                    VerifiedRestoreOutcome::RestoredFromConverged,
                );
            }
        }

        // Foreign tip: refuse to reset. The pre-abort ref was already
        // written by the caller (run_abort writes it for every repo before
        // calling this), so the tip is preserved either way; we surface
        // the label so the refusal message can name it.
        let pre_abort = self.resolve_pre_abort_ref(repo, op_id).unwrap_or_else(|| {
            // Should not happen: run_abort writes the pre-abort ref before
            // every verified_restore_savepoint call. Synthesise a label so
            // the refusal still names a recovery anchor.
            PreAbortRef {
                label: pre_abort_ref(op_id),
                revision: head.clone(),
            }
        });
        Ok(VerifiedRestoreOutcome::ForeignTip {
            observed_tip: head_sha.to_owned(),
            savepoint: savepoint.as_str().to_owned(),
            recorded_converged_tip: recorded_converged_tip.map(str::to_owned),
            pre_abort_ref: pre_abort,
        })
    }

    fn mid_op(&self, repo: &Path) -> Option<ConflictOp> {
        match Self::mid_op_state(repo).as_deref() {
            Some("mid-rebase") => Some(ConflictOp::Rebase),
            Some("mid-merge") => Some(ConflictOp::Merge),
            Some("mid-cherry-pick") => Some(ConflictOp::CherryPick),
            _ => None,
        }
    }

    fn cancel_in_flight_op(&self, repo: &Path) {
        let abort_args: &[&str] = match self.mid_op(repo) {
            Some(ConflictOp::Rebase) => &["rebase", "--abort"],
            Some(ConflictOp::Merge) => &["merge", "--abort"],
            Some(ConflictOp::CherryPick) => &["cherry-pick", "--abort"],
            None => return,
        };
        let _ = Self::run(abort_args, repo);
    }

    fn branch_has_remote_counterpart(
        &self,
        repo: &Path,
        branch: &RefName,
        role: Role,
    ) -> Result<bool, VcsError> {
        let _ = role; // all remotes use `origin`
        let qualified = format!("refs/remotes/origin/{}", branch.as_str());
        let status = git_command()
            .args(["rev-parse", "--verify", "--quiet", &qualified])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| VcsError::Io {
                ctx: format!(
                    "failed to spawn git rev-parse --verify --quiet {} in {}",
                    qualified,
                    repo.display()
                ),
                source: e,
            })?;
        Ok(status.success())
    }

    fn count_commits_ahead_of_remote(
        &self,
        repo: &Path,
        branch: &RefName,
        role: Role,
    ) -> Result<usize, VcsError> {
        let _ = role; // all remotes use `origin`
        let range = format!(
            "refs/remotes/origin/{}..{}",
            branch.as_str(),
            branch.as_str()
        );
        let out = Self::run(&["rev-list", "--count", &range], repo)?;
        Ok(out.trim().parse::<usize>().unwrap_or(0))
    }

    fn list_local_branches(&self, repo: &Path) -> Result<Vec<RefName>, VcsError> {
        let output = Self::run(
            &["for-each-ref", "--format=%(refname)", "refs/heads/"],
            repo,
        )?;
        let branches = output
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| RefName::new(l.to_owned()))
            .collect();
        Ok(branches)
    }

    fn fetch_objects_from(&self, dst_repo: &Path, src_repo: &Path) {
        let src_path = src_repo.to_string_lossy().into_owned();
        // Errors are swallowed by design — for sibling worktrees that
        // share an object store the fetch may fail (FETCH_HEAD unavailable)
        // and yet the objects are already reachable. A real problem
        // surfaces at the subsequent operation (e.g. the ff merge in
        // sync-to step 3) which inspects the same objects.
        let _ = git_command()
            .args(["fetch", &src_path, "HEAD"])
            .current_dir(dst_repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    fn refresh_index_to_head_if_safe(&self, repo: &Path) {
        // Quick exit: index already matches HEAD.
        let clean = git_command()
            .args(["diff-index", "--cached", "--exit-code", "HEAD"])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true); // assume clean on error; never touch if unsure
        if clean {
            return;
        }

        // Get the current index tree SHA.
        let index_tree = match git_command().arg("write-tree").current_dir(repo).output() {
            Ok(out) if out.status.success() => String::from_utf8(out.stdout)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            _ => return, // can't verify — leave index alone
        };

        // Safety check: is the index tree the tree of some recent ancestor commit?
        // Bounded to last 200 commits to keep doctor fast on large histories.
        let ancestor_trees = match git_command()
            .args(["log", "--format=%T", "-200", "HEAD"])
            .current_dir(repo)
            .output()
        {
            Ok(out) if out.status.success() => String::from_utf8(out.stdout).unwrap_or_default(),
            _ => return,
        };

        if !ancestor_trees.lines().any(|t| t.trim() == index_tree) {
            return; // live staged content — do not clobber
        }

        // Safe: realign index to HEAD.
        let _ = git_command().arg("reset").current_dir(repo).output();
    }

    fn refresh_working_tree_to_head_if_safe(&self, repo: &Path) {
        // Quick exit: working tree already matches HEAD.
        let clean = git_command()
            .args(["diff-index", "--exit-code", "HEAD"])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true);
        if clean {
            return;
        }

        // Use --name-status: D = deleted from WT (always safe); M = modified (check blob).
        let status_out = match git_command()
            .args(["diff-index", "--name-status", "HEAD"])
            .current_dir(repo)
            .output()
        {
            Ok(out) if out.status.success() => out,
            _ => return,
        };
        let mut all_files: Vec<String> = Vec::new(); // all entries to restore
        let mut modified_files: Vec<String> = Vec::new(); // M entries needing blob check
        let mut has_entries = false;
        for line in String::from_utf8_lossy(&status_out.stdout).lines() {
            if line.is_empty() {
                continue;
            }
            has_entries = true;
            let mut parts = line.splitn(2, '\t');
            let status = parts.next().unwrap_or("").trim();
            let path = parts.next().unwrap_or("").trim();
            match status {
                "D" => {
                    all_files.push(path.to_owned());
                }
                "M" | "T" => {
                    all_files.push(path.to_owned());
                    modified_files.push(path.to_owned());
                }
                _ => return, // unknown status — leave working tree alone
            }
        }
        if !has_entries || all_files.is_empty() {
            return;
        }

        // For M files, verify the on-disk blob is reachable before touching anything.
        if !modified_files.is_empty() {
            let objects_out = match git_command()
                .args(["rev-list", "--objects", "-n", "200", "HEAD"])
                .current_dir(repo)
                .output()
            {
                Ok(out) if out.status.success() => out,
                _ => return,
            };
            let reachable: std::collections::HashSet<String> =
                String::from_utf8(objects_out.stdout)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| l.split_whitespace().next().map(|s| s.to_owned()))
                    .collect();
            for file in &modified_files {
                let hash_out = match git_command()
                    .args(["hash-object", file])
                    .current_dir(repo)
                    .output()
                {
                    Ok(out) if out.status.success() => out,
                    _ => return,
                };
                let blob_sha = String::from_utf8_lossy(&hash_out.stdout).trim().to_owned();
                if !reachable.contains(&blob_sha) {
                    return; // live edits — do not clobber
                }
            }
        }

        // Safe: restore all files from HEAD.
        let mut args = vec!["checkout".to_owned(), "HEAD".to_owned(), "--".to_owned()];
        args.extend(all_files);
        let _ = git_command().args(&args).current_dir(repo).output();
    }

    fn remote_url(&self, repo: &Path, remote: &str) -> Result<Option<String>, VcsError> {
        let output = git_command()
            .args(["remote", "get-url", remote])
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git remote get-url {remote}"),
                source: e,
            })?;

        if !output.status.success() {
            // "No such remote" → remote absent, not an error.
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("No such remote") || stderr.contains("no such remote") {
                return Ok(None);
            }
            return Err(VcsError::CommandFailed {
                args: vec!["remote".into(), "get-url".into(), remote.into()],
                repo: repo.to_path_buf(),
                stderr: stderr.into_owned(),
            });
        }

        let url = String::from_utf8(output.stdout)
            .map(|s| s.trim().to_string())
            .map_err(|_| VcsError::CommandFailed {
                args: vec!["remote".into(), "get-url".into(), remote.into()],
                repo: repo.to_path_buf(),
                stderr: "git output not valid UTF-8".into(),
            })?;
        Ok(Some(url))
    }

    fn commit_object_exists(&self, repo: &Path, sha: &str) -> Result<bool, VcsError> {
        let deref = format!("{sha}^{{commit}}");
        let status = git_command()
            .args(["cat-file", "-e", &deref])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git cat-file -e {sha}^{{commit}}"),
                source: e,
            })?;
        Ok(status.success())
    }

    fn resolve_canonical_store(&self, workspace: &Path) -> Option<PathBuf> {
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
        let raw = Self::run(
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            workspace,
        )
        .ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(PathBuf::from(trimmed))
    }

    fn list_stale_worktree_registrations(&self, repo: &Path) -> Result<Vec<PathBuf>, VcsError> {
        // `git worktree list --porcelain` emits one record per registration,
        // separated by blank lines. The lines we care about within a record:
        //   `worktree <path>`  — the registered worktree path
        //   `prunable <reason>` — present when the path no longer exists
        //                         (or the gitdir file points to a missing
        //                         location); marks the record for pruning.
        // We collect the `worktree` path of every record that carries a
        // `prunable` line.
        let output = Self::run(&["worktree", "list", "--porcelain"], repo)?;
        let mut stale: Vec<PathBuf> = Vec::new();
        let mut current_path: Option<PathBuf> = None;
        let mut current_prunable = false;
        for line in output.lines() {
            if line.is_empty() {
                if current_prunable {
                    if let Some(p) = current_path.take() {
                        stale.push(p);
                    }
                }
                current_path = None;
                current_prunable = false;
                continue;
            }
            if let Some(rest) = line.strip_prefix("worktree ") {
                current_path = Some(PathBuf::from(rest));
            } else if line == "prunable" || line.starts_with("prunable ") {
                current_prunable = true;
            }
        }
        // Flush the final record (porcelain output may not end with a blank line).
        if current_prunable {
            if let Some(p) = current_path.take() {
                stale.push(p);
            }
        }
        Ok(stale)
    }

    fn list_savepoint_op_ids(&self, repo: &Path) -> Result<Vec<String>, VcsError> {
        // `git for-each-ref` over `refs/rwv/pre-op/` returns every savepoint
        // ref this repo holds. Strip the namespace prefix to recover the
        // opaque op-id the caller originally supplied to `create_savepoint`.
        let output = Self::run(
            &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-op/"],
            repo,
        )?;
        let op_ids = output
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter_map(|l| l.strip_prefix("refs/rwv/pre-op/").map(str::to_owned))
            .collect();
        Ok(op_ids)
    }

    fn read_file_at_revision(
        &self,
        repo: &Path,
        revision: &ResolvedRevisionId,
        file_path: &Path,
    ) -> Result<String, VcsError> {
        let path_str = file_path.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!("file path {} is not valid UTF-8", file_path.display()),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-utf8 file path"),
        })?;
        // `git show <rev>:<path>` prints the blob at the given revision.
        // Non-zero exit when the revision is missing or the file doesn't
        // exist at that revision; both surface as CommandFailed and the
        // caller can inspect stderr.
        let rev_path = format!("{}:{}", revision.as_str(), path_str);
        match Self::run(&["show", &rev_path], repo) {
            Ok(content) => Ok(content),
            Err(VcsError::CommandFailed { stderr, .. })
                if is_revision_not_found(&stderr)
                    || stderr.contains("does not exist")
                    || stderr.contains("exists on disk")
                    || stderr.contains("Not a valid object") =>
            {
                Err(VcsError::RevisionNotFound {
                    repo: repo.to_path_buf(),
                    rev: rev_path,
                })
            }
            Err(e) => Err(e),
        }
    }

    fn rebase_stopped_commit_detail(&self, repo: &Path) -> String {
        GitVcs::rebase_stopped_commit_detail_impl(repo)
    }

    fn log_oneline_range(
        &self,
        repo: &Path,
        from: &str,
        to: &str,
        cap: usize,
    ) -> (Vec<String>, usize) {
        GitVcs::log_oneline_range_impl(repo, from, to, cap)
    }

    fn ahead_behind(&self, repo: &Path, savepoint: &str, tip: &str) -> (usize, usize) {
        GitVcs::ahead_behind_impl(repo, savepoint, tip)
    }

    fn unique_commits(
        &self,
        repo: &Path,
        parent_tip: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError> {
        // Resolve this workweave's current tip, then list `parent..tip`. The
        // range excludes anything the parent already has, so a parent that
        // advanced after the fork does not pollute the result.
        let tip = Self::rev_parse(repo, "HEAD")?;
        Self::commits_in_range(repo, parent_tip.as_str(), &tip)
    }

    fn unique_diff(
        &self,
        repo: &Path,
        parent_tip: &ResolvedRevisionId,
    ) -> Result<UniqueDiff, VcsError> {
        // Anchor the diff at the common ancestor of the tip and the parent
        // tip — NOT the parent tip directly. If the parent advanced after the
        // fork, diffing against its tip would show the parent's later work as
        // phantom deletions; the merge-base is the fork point, so the diff is
        // exactly this workweave's unique changes.
        let tip = Self::rev_parse(repo, "HEAD")?;
        let base = Self::merge_base(repo, parent_tip.as_str(), &tip)?;
        let text = Self::diff_range(repo, &base, &tip)?;
        Ok(UniqueDiff {
            base: Some(base),
            text,
        })
    }

    // =======================================================================
    // The branch model (branch-model.md §4.3)
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
        match Self::run(&["symbolic-ref", "HEAD"], repo) {
            Ok(full) => {
                let name = Self::local_branch_name_from_full_ref(full.trim(), repo)?;
                // HEAD is symbolic. Whether the branch has commits is a
                // second question: `symbolic-ref` succeeds on an unborn
                // branch, and `rev-parse HEAD` is what tells the two apart.
                // (This is the check `head_revision` had to grow inline; it
                // belongs here, where the question is actually asked.)
                match Self::run(&["rev-parse", "--verify", "HEAD^{commit}"], repo) {
                    Ok(_) => Ok(HeadObservation::Attached { name }),
                    Err(_) => Ok(HeadObservation::Unborn { name }),
                }
            }
            Err(symbolic_err) => {
                // HEAD is not symbolic — or the ref database is unreadable.
                // Resolving HEAD tells us which: a detached HEAD resolves,
                // a broken refdb does not, and the latter is an error.
                match Self::run(&["rev-parse", "--verify", "HEAD^{commit}"], repo) {
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
        // Fully qualified: `refs/heads/<name>` cannot be answered by a tag
        // or a remote-tracking ref of the same name.
        let qualified = format!("refs/heads/{}^{{commit}}", name.as_str());
        match Self::run(&["rev-parse", "--verify", "--quiet", &qualified], &repo) {
            Ok(sha) => Ok(ResolvedRevisionId::from_rev_parse_output(&sha)),
            // `--quiet` makes "no such ref" an exit code with no stderr,
            // which is the absence this method reports rather than an error.
            Err(VcsError::CommandFailed { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn mid_operation(&self, repo: &Path) -> Option<String> {
        Self::mid_op_state(repo)
    }

    fn materialize_worktree_on_ref(
        &self,
        store: &Path,
        dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<bool, VcsError> {
        let store = Self::work_dir_for_store(store);
        let dest_str = dest.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!("worktree path {} is not valid UTF-8", dest.display()),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-utf8 worktree path"),
        })?;
        let start = start_point.as_str();
        let branch = name.as_str();

        // Classify BEFORE acting, by asking whether the ref is there. Two
        // reasons it cannot be done by matching stderr afterwards: git says
        // "already exists" for a taken destination path as well as for a
        // taken branch name, and `worktree add -b` creates the branch
        // before it fails on the destination, so a post-hoc look sees a ref
        // this very call just made.
        if self.resolve_local_branch_tip(&store, name)?.is_some() {
            // ADOPT. The shipped path force-deleted the branch here and
            // retried with -b, destroying a ref on nothing but a name match
            // — a DESTROY needs a receipt and a warrant (R2, R3), and this
            // call holds neither, so it cannot be reached from here.
            Self::run(&["worktree", "add", dest_str, branch], &store)?;
            return Ok(false);
        }
        // AUTHOR. If this fails after git has already written the ref (a
        // taken destination is the usual way), the ref is left in place
        // rather than cleaned up: the receipt was persisted before this
        // call, so what remains is a recorded ref with no worktree, which
        // doctor can reconcile. Deleting it here would be an unwarranted
        // DESTROY, which is the failure mode this path exists to remove.
        Self::run(&["worktree", "add", "-b", branch, dest_str, start], &store)?;
        Ok(true)
    }

    fn clone_attached_at(
        &self,
        url: &str,
        dest: &Path,
        role: Role,
        name: &LocalRefName,
        at: &RawRevisionId,
    ) -> Result<ResolvedRevisionId, VcsError> {
        let _ = role; // role label kept for signal value; all clones use `origin`
        let dest_str = dest.to_str().ok_or_else(|| VcsError::Io {
            ctx: format!("destination path {} is not valid UTF-8", dest.display()),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "non-utf8 destination path",
            ),
        })?;
        // `--no-checkout`: the working tree is never materialized at the
        // remote tip. Cloning still writes the remote's default branch as a
        // local ref — git offers no way to decline that — but no working
        // tree hangs off it, nothing has observed it, and rwv issues no
        // MOVE against it. That last part is the property §5 needs:
        // bootstrapping from a lock behind origin must not require a
        // rewind's consent.
        Self::run(
            &[
                "clone",
                "--origin",
                "origin",
                "--no-checkout",
                url,
                dest_str,
            ],
            Path::new("."),
        )?;
        // Resolved in the new clone's own object store, so a pin origin has
        // but this clone does not is a resolution failure rather than a
        // silent network fetch — the same locality the present-clone arm has.
        let at = self.resolve_revision(dest, at.as_str())?;
        // `-B` names the ref the clone just minted; with an explicit start
        // point there is no `checkout.guess` and no tag lookup, and the `--`
        // terminator keeps a path-shaped `name` from being read as a
        // pathspec — the three misreads `attach_head_to` documents.
        Self::run(&["checkout", "-B", name.as_str(), at.as_str(), "--"], dest)?;
        Ok(at)
    }

    fn set_detached_head(&self, repo: &Path, to: &ResolvedRevisionId) -> Result<(), VcsError> {
        Self::run(&["checkout", "--detach", to.as_str()], repo)?;
        Ok(())
    }

    fn attach_head_to(&self, repo: &Path, name: &LocalRefName) -> Result<(), VcsError> {
        // Classify before acting, the way `materialize_worktree_on_ref`
        // does. Omitting `-b` does NOT make git refuse an absent branch —
        // measured, all three on git 2.43:
        //
        //   * `checkout.guess` (on by default) invents a local branch from
        //     a remote-tracking one of the same name and reports "Switched
        //     to a new branch". That is a birth with no receipt, so the ref
        //     is unowned under R2 forever. It is also the ordinary case
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
        // what makes the refusal real; the flags are defence in depth for
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
                args: vec![
                    "rev-parse".to_owned(),
                    "--verify".to_owned(),
                    "--quiet".to_owned(),
                    format!("refs/heads/{name}^{{commit}}"),
                ],
                repo: Self::work_dir_for_store(repo),
                stderr: format!(
                    "no local branch named '{name}': attaching to a branch that \
                     does not exist would create it, and a birth needs a receipt \
                     so the ref can be owned"
                ),
            });
        }
        Self::run(&["checkout", "--no-guess", name.as_str(), "--"], repo)?;
        Ok(())
    }

    fn destroy_local_ref(&self, store: &Path, name: &RawRefName) -> Result<(), VcsError> {
        let store = Self::work_dir_for_store(store);
        Self::run(&["branch", "-D", name.as_str()], &store)?;
        Ok(())
    }

    fn rename_local_ref(
        &self,
        store: &Path,
        from: &RawRefName,
        to: &RawRefName,
    ) -> Result<(), VcsError> {
        let store = Self::work_dir_for_store(store);
        // `-m`, never `-M`. The uppercase form renames *over* an existing
        // branch, which destroys that branch's ref with neither receipt nor
        // warrant; the lowercase form refuses, which is the behaviour the
        // trait's contract requires. git also refuses the D/F direction on
        // its own ("cannot lock ref"), so a leftover `p--w/other` blocks the
        // rename rather than being silently swept up.
        //
        // Run from the store's work dir, not from the worktree whose HEAD is
        // on `from`: `git branch -m` updates the HEAD of every worktree that
        // pointed at the old name, wherever it is run.
        Self::run(&["branch", "-m", from.as_str(), to.as_str()], &store)?;
        Ok(())
    }

    fn birth_ref_at_head(&self, repo: &Path, name: &RawRefName) -> Result<(), VcsError> {
        // Classify before acting, like `attach_head_to` above: `switch -c`
        // on an existing name exits non-zero, but the refusal this contract
        // owes is "rwv holds no receipt for that ref", not git's "already
        // exists" — and stating it here keeps the reason on the model's
        // terms rather than on git's.
        if self.resolve_local_branch_tip(repo, name)?.is_some() {
            return Err(VcsError::CommandFailed {
                args: vec![
                    "rev-parse".to_owned(),
                    "--verify".to_owned(),
                    "--quiet".to_owned(),
                    format!("refs/heads/{name}^{{commit}}"),
                ],
                repo: repo.to_path_buf(),
                stderr: format!(
                    "a local branch named '{name}' already exists: birthing it here \
                     would adopt a ref this call holds no receipt for, and moving it \
                     to HEAD would be an unwitnessed MOVE"
                ),
            });
        }
        // No start point: the ref is born where HEAD already is, so the
        // working tree does not move.
        Self::run(&["switch", "-c", name.as_str()], repo)?;
        Ok(())
    }

    fn push_ref(
        &self,
        repo: &Path,
        role: Role,
        r: &PublishRef,
        force: bool,
    ) -> Result<(), VcsError> {
        let _ = role; // all remotes use `origin`
        let mut args: Vec<&str> = vec!["push"];
        if force {
            args.push("--force");
        }
        args.push("origin");
        args.push(r.name().as_str());
        Self::run(&args, repo)?;
        Ok(())
    }

    fn remote_default_branch(&self, repo: &Path) -> Result<Option<RemoteDefaultBranch>, VcsError> {
        // A non-repo is an error; an unset symref is an absence. Keeping
        // those apart is the same move `observe_head` makes, applied to the
        // other side of the L1 publish gate.
        if !self.is_repo(repo) {
            return Err(VcsError::NotARepo(repo.to_path_buf()));
        }
        const NAMESPACE: &str = "refs/remotes/origin/";
        match Self::run(&["symbolic-ref", "refs/remotes/origin/HEAD"], repo) {
            // No fallback. The shipped `default_branch` invented "main"
            // here, so the publish gate compared an observation against a
            // guess; `None` makes the gate refuse and say what is missing.
            Ok(target) => Ok(RemoteDefaultBranch::from_symref_target(&target, NAMESPACE)),
            Err(_) => Ok(None),
        }
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
        // `lstrip=2`, not `short` — see `list_branch_names_with_prefix`.
        let output = Self::run(
            &[
                "for-each-ref",
                "--format=%(refname:lstrip=2)",
                "refs/heads/",
            ],
            repo,
        )?;
        Ok(output
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(RawRefName::new)
            .collect())
    }
}

/// Build the savepoint ref path for `op_id` under the rwv pre-op namespace.
///
/// The namespacing (`refs/rwv/pre-op/<id>`) is a git impl detail —
/// callers of the [`Vcs`] trait pass an opaque `op_id` string and never
/// spell the ref directly. Centralising the format here means create /
/// resolve / drop / restore all agree on the layout.
fn savepoint_ref(op_id: &str) -> String {
    format!("refs/rwv/pre-op/{op_id}")
}

/// Build the pre-abort ref path for `op_id` under the rwv pre-abort namespace.
///
/// The namespacing (`refs/rwv/pre-abort/<id>`) is a git impl detail —
/// callers of the [`Vcs`] trait receive a [`PreAbortRef`] whose `label`
/// carries this string for recovery hints, but never spell the ref
/// directly. Centralising the format here keeps create / resolve in sync.
fn pre_abort_ref(op_id: &str) -> String {
    format!("refs/rwv/pre-abort/{op_id}")
}

impl GitVcs {
    /// Reset `repo` to `savepoint` via `git reset --hard`, drop the savepoint
    /// ref, and return `outcome`. This factors out the identical reset+drop
    /// sequence shared by the intent, converged, and mid-op restore branches
    /// of [`Vcs::verified_restore_savepoint`].
    fn reset_and_drop_savepoint(
        &self,
        repo: &Path,
        op_id: &str,
        savepoint: &ResolvedRevisionId,
        outcome: VerifiedRestoreOutcome,
    ) -> Result<VerifiedRestoreOutcome, VcsError> {
        Self::run(&["reset", "--hard", savepoint.as_str()], repo)?;
        self.drop_savepoint(repo, op_id);
        Ok(outcome)
    }
}

impl GitVcs {
    /// Return the list of dirty file paths in `repo` as reported by
    /// `git status --porcelain`.
    ///
    /// Each entry is the path portion of the status line after the two-char
    /// status code and its trailing space. Returns an empty vec when the tree
    /// is clean; returns an `Err` only when git itself fails.
    ///
    /// **Parsing note:** `run()` trims the overall output, which can strip the
    /// leading space of a single-entry `" M filename"` result. We normalize
    /// each line by trimming leading spaces before extracting the path so that
    /// both `"?? file"` and `"M file"` (after trim) parse correctly: skip the
    /// first two non-space characters (the XY status code) and any following
    /// whitespace to obtain the filename.
    pub(crate) fn dirty_file_names(repo: &Path) -> Result<Vec<String>, VcsError> {
        Self::dirty_file_names_inner(repo, false)
    }

    /// Return the list of dirty **tracked** file paths in `repo` — staged and
    /// unstaged modifications to files git already tracks, with untracked files
    /// excluded (`git status --porcelain --untracked-files=no`).
    ///
    /// This is the source-side cleanliness signal: a
    /// `sync-to` refuses up-front on tracked dirt (it would go stale mid-rebase)
    /// but leaves untracked scratch files alone (they survive the replay). The
    /// parsing contract matches [`dirty_file_names`]; only the untracked class
    /// is filtered out at the git level.
    pub(crate) fn tracked_dirty_file_names(repo: &Path) -> Result<Vec<String>, VcsError> {
        Self::dirty_file_names_inner(repo, true)
    }

    fn dirty_file_names_inner(repo: &Path, tracked_only: bool) -> Result<Vec<String>, VcsError> {
        let output = if tracked_only {
            Self::run(&["status", "--porcelain", "--untracked-files=no"], repo)?
        } else {
            Self::run(&["status", "--porcelain"], repo)?
        };
        if output.is_empty() {
            return Ok(Vec::new());
        }
        let names = output
            .lines()
            .filter_map(|line| {
                // Strip leading spaces introduced by run()'s global trim on
                // single-entry output (e.g. " M rwv.yaml" → "M rwv.yaml").
                let trimmed = line.trim_start();
                // Porcelain v1: XY + space + path. After trim_start the XY
                // code is at most 2 chars; skip them, then strip the space.
                trimmed
                    .get(2..)
                    .map(|s| s.trim_start_matches(' '))
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .collect();
        Ok(names)
    }
}

#[cfg(test)]
mod branch_model_tests {
    //! Receipts, warrants, and consent-gated attachment changes.
    //!
    //! These live in-crate because the types they exercise are minted by
    //! crate-internal producers: a receipt comes from the registry, a
    //! consent token from the flag module. That is the point — an
    //! integration test *cannot* forge them, which is the invariant.
    //!
    //! Everything reachable from outside the crate is in
    //! `tests/branch_model_test.rs`.

    use super::*;
    use crate::cli::consent::{DetachConsent, DiscardUnmergedConsent, ReattachConsent};
    use crate::vcs::{
        DeletionWarrant, DiscardLocalCommitsConsent, DiscardWarrant, OwnedRef, RawRefName,
        TrackingRef,
    };

    /// A local branch name, obtained the only way one can be: through the
    /// named projection off a declared tracking ref.
    fn local(name: &str) -> LocalRefName {
        TrackingRef::parse(RawRefName::new(name))
            .expect("test fixture names are valid tracking refs")
            .local_counterpart()
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
        let tmp = tempfile::tempdir().unwrap();
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
        let tmp = tempfile::tempdir().unwrap();
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

    /// A receipt as the registry would persist one: store, name, and the
    /// tip the ref was recorded at.
    fn receipt(store: &Path, name: &str, at: &ResolvedRevisionId) -> OwnedRef {
        OwnedRef::from_receipt(store.to_path_buf(), RawRefName::new(name), at.clone())
    }

    /// A worktree destination that does not exist yet, inside its own temp
    /// directory so parallel tests cannot collide on it.
    fn worktree_dest() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
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

        let born = GitVcs
            .create_worktree_on(&receipt(store.path(), "p--ww", &start), &dest)
            .unwrap()
            .expect("this call created the ref");

        assert_eq!(born.name().as_str(), "p--ww");
        assert_eq!(born.at(), &start);
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
        // needs a receipt and a warrant; this path has neither, so it must
        // adopt.
        let store = repo();
        let start = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        git(store.path(), &["checkout", "p--ww"]);
        let unique = commit(store.path(), "unique-work");
        git(store.path(), &["checkout", "main"]);

        let (_dest_home, dest) = worktree_dest();
        let born = GitVcs
            .create_worktree_on(&receipt(store.path(), "p--ww", &start), &dest)
            .unwrap();

        assert!(born.is_none(), "adopted, not authored");
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

    // -----------------------------------------------------------------------
    // DESTROY: receipt + warrant
    // -----------------------------------------------------------------------

    #[test]
    fn unmoved_holds_only_while_the_ref_is_where_the_receipt_recorded_it() {
        let store = repo();
        let recorded = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        let r = receipt(store.path(), "p--ww", &recorded);

        assert!(
            DeletionWarrant::unmoved(&GitVcs, &r).is_some(),
            "tip still equals the recorded tip"
        );

        // Someone commits on it. The branch is no longer stale, and the
        // warrant that licensed deleting a stale branch evaporates.
        git(store.path(), &["checkout", "p--ww"]);
        commit(store.path(), "work");
        git(store.path(), &["checkout", "main"]);
        assert!(DeletionWarrant::unmoved(&GitVcs, &r).is_none());
    }

    #[test]
    fn unmoved_is_none_for_a_receipt_whose_ref_does_not_exist() {
        let store = repo();
        let recorded = GitVcs.head_revision(store.path()).unwrap();
        let r = receipt(store.path(), "never-created", &recorded);
        assert!(DeletionWarrant::unmoved(&GitVcs, &r).is_none());
    }

    #[test]
    fn merged_holds_exactly_when_the_tip_is_reachable_from_the_baseline() {
        let store = repo();
        let base = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        let r = receipt(store.path(), "p--ww", &base);

        // main advances past the branch: the branch's tip is an ancestor.
        let advanced = commit(store.path(), "later");
        assert!(DeletionWarrant::merged(&GitVcs, &r, &advanced).is_some());

        // The branch gains a commit the baseline does not have.
        git(store.path(), &["checkout", "p--ww"]);
        commit(store.path(), "branch-only");
        git(store.path(), &["checkout", "main"]);
        assert!(DeletionWarrant::merged(&GitVcs, &r, &advanced).is_none());
    }

    #[test]
    fn delete_owned_ref_destroys_the_ref_the_receipt_names() {
        let store = repo();
        let recorded = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        let r = receipt(store.path(), "p--ww", &recorded);

        let warrant = DeletionWarrant::unmoved(&GitVcs, &r).expect("unmoved");
        GitVcs.delete_owned_ref(&r, warrant).unwrap();

        assert!(GitVcs
            .resolve_local_branch_tip(store.path(), &RawRefName::new("p--ww"))
            .unwrap()
            .is_none());
        assert!(
            GitVcs
                .resolve_local_branch_tip(store.path(), &RawRefName::new("main"))
                .unwrap()
                .is_some(),
            "nothing else was touched"
        );
    }

    #[test]
    fn a_receipt_keyed_to_a_git_dir_still_names_a_workable_repo() {
        // Receipts are keyed by canonical store, which `resolve_canonical_store`
        // reports as `<clone>/.git` — not the working directory.
        let store = repo();
        let recorded = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        let r = receipt(&store.path().join(".git"), "p--ww", &recorded);

        let warrant = DeletionWarrant::unmoved(&GitVcs, &r).expect("unmoved via .git path");
        GitVcs.delete_owned_ref(&r, warrant).unwrap();
        assert!(GitVcs
            .resolve_local_branch_tip(store.path(), &RawRefName::new("p--ww"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn an_unmerged_ref_is_destroyed_only_on_the_operator_s_say_so() {
        let store = repo();
        let base = GitVcs.head_revision(store.path()).unwrap();
        git(store.path(), &["branch", "p--ww"]);
        git(store.path(), &["checkout", "p--ww"]);
        commit(store.path(), "unmerged-work");
        git(store.path(), &["checkout", "main"]);
        let r = receipt(store.path(), "p--ww", &base);

        // Neither structural warrant holds: the ref moved, and its tip is
        // not reachable from the baseline.
        assert!(DeletionWarrant::unmoved(&GitVcs, &r).is_none());
        assert!(DeletionWarrant::merged(&GitVcs, &r, &base).is_none());

        // The named override is the only remaining route, and it is a
        // token the operator has to have produced.
        let warrant = DeletionWarrant::operator_discarded(DiscardUnmergedConsent::granted());
        GitVcs.delete_owned_ref(&r, warrant).unwrap();
        assert!(GitVcs
            .resolve_local_branch_tip(store.path(), &RawRefName::new("p--ww"))
            .unwrap()
            .is_none());
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
            .create_worktree_on(&receipt(store.path(), "p--ww", &start), &dest)
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
        // at the start point and NOT destroyed on the way out: the receipt
        // for it was persisted before this call, so a recorded ref with no
        // worktree is reconcilable, while an unwarranted delete is not
        // recoverable at all.
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
            .detach_head(&w, &target, DetachConsent::granted())
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
            .detach_head(&w, &target, DetachConsent::granted())
            .unwrap_err();
        assert_eq!(err.kind(), "stale-ref-witness");
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
        git(p.path(), &["checkout", "--detach", tip.as_str()]);

        let from = GitVcs.head_attachment(p.path()).unwrap();
        GitVcs
            .reattach_head(from, &local("target"), ReattachConsent::granted())
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
        git(p.path(), &["checkout", "--detach", tip.as_str()]);

        let from = GitVcs.head_attachment(p.path()).unwrap();
        // The operator reattaches by hand first.
        git(p.path(), &["checkout", "main"]);

        let err = GitVcs
            .reattach_head(from, &local("target"), ReattachConsent::granted())
            .unwrap_err();
        assert_eq!(err.kind(), "stale-ref-witness");
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
        // `version:` could plausibly declare.
        let origin = repo();
        git(origin.path(), &["branch", "feature"]);
        let (_home, work) = clone_of(origin.path());
        git(&work, &["tag", "tagname"]);
        std::fs::create_dir(work.join("docs")).unwrap();
        std::fs::write(work.join("docs/a.md"), "committed").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "docs"]);

        let tip = GitVcs.head_revision(&work).unwrap();
        git(&work, &["checkout", "--detach", tip.as_str()]);
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
                .reattach_head(from, &local(name), ReattachConsent::granted())
                .unwrap_err();

            assert_eq!(err.kind(), "command-failed", "{name}: expected a refusal");
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
            .reattach_head(from, &local("absent"), ReattachConsent::granted())
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
        git(&work, &["checkout", "--detach", tip.as_str()]);
        let from = GitVcs.head_attachment(&work).unwrap();

        GitVcs
            .reattach_head(from, &local("shared"), ReattachConsent::granted())
            .unwrap();

        assert_eq!(
            GitVcs.head_attachment(&work).unwrap().to_string(),
            "on branch 'shared'",
            "the branch wins over the tag, the remote ref and the path"
        );
    }

    // -----------------------------------------------------------------------
    // Rewinding MOVE: the warrant must belong to the repo being rewound
    // -----------------------------------------------------------------------

    #[test]
    fn reset_attached_ref_rewinds_behind_a_savepoint_taken_in_the_same_repo() {
        let p = repo();
        let base = GitVcs.head_revision(p.path()).unwrap();
        let _ = commit(p.path(), "to-discard");

        let savepoint = GitVcs.create_savepoint_ref(p.path(), "op-1").unwrap();
        let warrant = DiscardWarrant::new(savepoint, DiscardLocalCommitsConsent::granted());
        let HeadAttachment::Attached(w) = GitVcs.head_attachment(p.path()).unwrap() else {
            panic!("fixture should be attached");
        };

        GitVcs.reset_attached_ref(&w, &base, warrant).unwrap();

        assert_eq!(GitVcs.head_revision(p.path()).unwrap(), base);
        assert_eq!(
            GitVcs.head_attachment(p.path()).unwrap().to_string(),
            "on branch 'main'",
            "a rewind is still a MOVE: the attachment is unchanged"
        );
    }

    #[test]
    fn reset_attached_ref_refuses_a_savepoint_taken_in_another_repo() {
        let a = repo();
        let b = repo();
        let a_base = GitVcs.head_revision(a.path()).unwrap();
        let a_tip = commit(a.path(), "to-discard");

        // A savepoint for B cannot license rewinding A: it captured a tip
        // that has nothing to do with A's history.
        let elsewhere = GitVcs.create_savepoint_ref(b.path(), "op-1").unwrap();
        let warrant = DiscardWarrant::new(elsewhere, DiscardLocalCommitsConsent::granted());
        let HeadAttachment::Attached(w) = GitVcs.head_attachment(a.path()).unwrap() else {
            panic!("fixture should be attached");
        };

        let err = GitVcs.reset_attached_ref(&w, &a_base, warrant).unwrap_err();
        assert_eq!(err.kind(), "stale-ref-witness");
        assert_eq!(
            GitVcs.head_revision(a.path()).unwrap(),
            a_tip,
            "the refusal is a refusal: nothing was discarded"
        );
    }

    // -----------------------------------------------------------------------
    // Unborn HEAD: a state, and not one a MOVE can reach
    // -----------------------------------------------------------------------

    #[test]
    fn an_unborn_head_yields_no_witness_to_move() {
        let tmp = tempfile::tempdir().unwrap();
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

    #[test]
    fn work_dir_for_store_normalises_a_git_dir_and_leaves_anything_else_alone() {
        let clone = Path::new("/w/github/acme/server");
        assert_eq!(GitVcs::work_dir_for_store(&clone.join(".git")), clone);
        assert_eq!(GitVcs::work_dir_for_store(clone), clone);
    }
}
