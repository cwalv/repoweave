//! Git implementation of the [`Vcs`] trait.

use crate::manifest::Role;
use crate::vcs::{
    ConflictOp, PreAbortRef, RefName, ResolvedRevisionId, Vcs, VcsError, VerifiedRestoreOutcome,
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

/// Git-based version control operations.
pub struct GitVcs;

/// One commit from a `git log <range>` listing.
///
/// Produced by [`GitVcs::commits_in_range`] for `rwv workweave log`. `sha` is
/// the full 40-hex SHA (stable identity for agents); `short` is the
/// abbreviated form; `subject` is the first line of the commit message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitEntry {
    /// Full 40-hex commit SHA.
    pub sha: String,
    /// Abbreviated commit SHA (as git chose the length).
    pub short: String,
    /// First line of the commit message.
    pub subject: String,
}

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
    /// Thin wrapper over `git rev-parse --verify <rev>^{commit}`. Returns the
    /// error string on failure (unknown revision, not a repo) so the caller can
    /// decide whether that repo is skippable. Used to resolve a parent tip in
    /// the parent's checkout for `rwv workweave log`.
    pub fn rev_parse(repo: &Path, rev: &str) -> Result<String, String> {
        let deref = format!("{rev}^{{commit}}");
        Self::run(&["rev-parse", "--verify", &deref], repo).map_err(|e| e.to_string())
    }

    /// Compute the merge-base of `a` and `b` in `repo`.
    ///
    /// Returns the common-ancestor SHA. Used by `rwv workweave diff` to anchor
    /// the whole-bead diff range at `git merge-base <parent-tip> HEAD` rather
    /// than the parent tip directly — diffing against a parent tip that
    /// advanced after the fork shows phantom reversals of other beads' changes.
    pub fn merge_base(repo: &Path, a: &str, b: &str) -> Result<String, String> {
        Self::run(&["merge-base", a, b], repo).map_err(|e| e.to_string())
    }

    /// List the commits reachable from `to` but not from `from` in `repo`,
    /// newest first, as `git log --oneline`-style `<short-sha> <subject>`
    /// lines.
    ///
    /// This is `git log <from>..<to>` semantics: with `from` = the parent tip
    /// and `to` = HEAD, the result is exactly the workweave's UNIQUE commits,
    /// and it stays correct when the parent advanced since the fork (the
    /// range excludes commits the parent already has). An empty vec means no
    /// unique commits.
    pub fn commits_in_range(repo: &Path, from: &str, to: &str) -> Result<Vec<CommitEntry>, String> {
        // `%H` full SHA, `%h` short SHA, `%s` subject — NUL-delimited fields,
        // newline-delimited records, so subjects with spaces/tabs survive.
        let range = format!("{from}..{to}");
        let fmt = "--pretty=format:%H%x00%h%x00%s";
        let out = Self::run(&["log", fmt, &range], repo).map_err(|e| e.to_string())?;
        let entries = out
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\0');
                let sha = parts.next()?.to_string();
                let short = parts.next()?.to_string();
                let subject = parts.next().unwrap_or("").to_string();
                Some(CommitEntry {
                    sha,
                    short,
                    subject,
                })
            })
            .collect();
        Ok(entries)
    }

    /// Produce the unified diff of `from..to` in `repo` (three-dot range so the
    /// diff is anchored at the merge-base of the endpoints when the caller
    /// passes a merge-base as `from`, this is a two-dot equivalent).
    ///
    /// Callers pass `from` = `git merge-base <parent-tip> HEAD` and `to` =
    /// HEAD, so the output is the whole-bead diff with no phantom reversals.
    pub fn diff_range(repo: &Path, from: &str, to: &str) -> Result<String, String> {
        let range = format!("{from}..{to}");
        Self::run(&["diff", &range], repo).map_err(|e| e.to_string())
    }

    /// Detect if a repo is in a mid-operation VCS state (mid-rebase, mid-merge, etc.).
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
/// [`crate::sync`] splices into its conflict-bail messages, sandwiched
/// between an opening "what happened" line and a closing "or `rwv abort`
/// to roll back" line. Kept as a free helper so the VCS impl is the sole
/// owner of git vocabulary; rwv core never spells "git add" or
/// "git rebase --continue".
fn git_conflict_resolution_hint(op: ConflictOp) -> String {
    let continue_cmd = match op {
        ConflictOp::Rebase => "git rebase --continue",
        ConflictOp::Merge => "git merge --continue",
        ConflictOp::CherryPick => "git cherry-pick --continue",
    };
    format!("  # edit conflicted files\n  git add <files>\n  {continue_cmd}")
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
fn is_release_shape_tag(tag: &str) -> bool {
    let rest = match tag.strip_prefix('v') {
        Some(r) => r,
        None => return false,
    };
    // Require at least "N.N" (e.g., "1.0") to count as release-shape.
    let mut parts = rest.split(['.', '-', '+']);
    let first = parts.next().unwrap_or("");
    let second = parts.next().unwrap_or("");
    !first.is_empty()
        && first.chars().all(|c| c.is_ascii_digit())
        && !second.is_empty()
        && second.chars().next().is_some_and(|c| c.is_ascii_digit())
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
        let sha = Self::run(&["rev-parse", "HEAD"], repo)?;
        // If a tag points at HEAD, preserve it as the display form so callers
        // get human-readable round-trips (e.g., `v0.3.4`) without an extra
        // resolve step.
        let display = self.tag_at_head(repo)?.map(|t| t.as_str().to_string());
        Ok(ResolvedRevisionId::from_canonical(sha, display))
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
        // Wire up the `ours` merge driver inline (no persistent
        // `.git/config` change) so the `merge=ours` lines written by
        // [`set_replay_exclusion`] resolve to "keep the rebase-target's
        // version" — `driver = true` is the shell command `true`, which
        // succeeds without modifying the merged file. Doing this per
        // invocation (rather than at `rwv init` time) means the driver is
        // available on every clone without per-clone setup.
        //
        // [`set_replay_exclusion`]: Vcs::set_replay_exclusion
        // `git rebase --onto <onto> <upstream>` replays commits in
        // <upstream>..HEAD onto <onto>. On conflict, git leaves the repo
        // mid-rebase (rebase-merge/ + conflict markers in WT). We detect
        // that state and surface VcsError::RebaseConflict so the caller can
        // pair with conflict_resolution_hint(ConflictOp::Rebase).
        // `--empty=drop`: drop commits that become empty after rebase. This
        // is what makes lock-only commits silently disappear when the
        // `merge=ours` driver on rwv.lock (configured via
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
        let output = git_command()
            .args([
                "-c",
                "merge.ours.name=keep ours during replay (rwv replay-exclusion)",
                "-c",
                "merge.ours.driver=true",
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
        let needle = format!("{path_str} merge=ours");

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

        if existing.lines().any(|line| line.trim() == needle) {
            return Ok(());
        }

        // Append, preserving any existing entries. Ensure exactly one
        // trailing newline before the new line so concatenation is clean
        // whether the file ended with a newline or not.
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&needle);
        next.push('\n');

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
        let needle = format!("{path_str} merge=ours");

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
        let needle = format!("{path_str} merge=ours");

        // `git show HEAD:.gitattributes` — if `.gitattributes` is not
        // committed at HEAD, git exits non-zero and we treat the line as
        // absent (the precondition concern is "is the line in the committed
        // tree?", so a missing file is a definitive No).
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
            return Ok(false);
        }
        let content = String::from_utf8_lossy(&output.stdout);
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
        // `git rev-parse <ref>` emits the canonical 40-hex SHA for a
        // fully-qualified ref, so the result is already in canonical form
        // and re-resolving via `resolve_revision` would add a git
        // invocation without strengthening the invariant. This is the
        // sole legitimate caller of
        // `ResolvedRevisionId::from_canonical_unchecked`; see that
        // constructor's doc-comment.
        let ref_name = savepoint_ref(op_id);
        Self::run(&["rev-parse", &ref_name], repo)
            .ok()
            .map(ResolvedRevisionId::from_canonical_unchecked)
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
        // Same canonical-rev-parse contract as `resolve_savepoint`: rev-parse
        // on a fully-qualified ref emits the canonical 40-hex SHA, so the
        // result is already in canonical form.
        let label = pre_abort_ref(op_id);
        let canonical = Self::run(&["rev-parse", &label], repo).ok()?;
        Some(PreAbortRef {
            revision: ResolvedRevisionId::from_canonical_unchecked(canonical),
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
    /// This is the source-side cleanliness signal (`fo-4rpnkm.1` §1): a
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
