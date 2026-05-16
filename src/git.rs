//! Git implementation of the [`Vcs`] trait.

use crate::vcs::{RefName, ResolvedRevisionId, Vcs, VcsError};
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
/// `savepoint/*` (operator/tool savepoints) and `rwv/pre-op/*` (sync abort
/// recovery refs under `refs/rwv/pre-op/*` when surfaced as tag names).
fn is_transient_tag(tag: &str) -> bool {
    tag.starts_with("savepoint/")
        || tag.starts_with("rwv/pre-op/")
        || tag.starts_with("refs/rwv/pre-op/")
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

        // Try the conventional `origin` first, then `upstream` (the remote
        // name rwv uses for role=fork clones). Strip the matching prefix to
        // recover the bare branch name.
        for remote in ["origin", "upstream"] {
            let sym = format!("refs/remotes/{remote}/HEAD");
            if let Ok(sym_ref) = Self::run(&["symbolic-ref", &sym], repo) {
                let prefix = format!("refs/remotes/{remote}/");
                let branch = sym_ref.strip_prefix(&prefix).unwrap_or(FALLBACK).to_string();
                return Ok(RefName::new(branch));
            }
        }
        Ok(RefName::new(FALLBACK))
    }
}
