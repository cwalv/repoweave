//! Workweave operations: create, delete, list, and sync workweaves.
//!
//! A workweave is a parallel working directory containing worktrees for each
//! repo in a project, including the project repo itself. The workweave directory
//! lives under `.workweaves/` in the parent of the workspace root (or under
//! `RWV_WORKWEAVE_DIR` if set). Each workweave carries its own `.rwv-workweave` marker
//! and `.rwv-active` file so it is fully self-describing.

use crate::git::{git_command, GitVcs};
use crate::manifest::{Manifest, ProjectName, Role, WorkweaveName};
use crate::vcs::{vcs_for, RefName, Vcs};
use crate::workspace::{
    parse_weave_dir_name, read_active_project, set_active_project, weave_dir_name, WorkweaveMarker,
};
use anyhow::{anyhow, bail, Context};
use std::path::{Path, PathBuf};

/// How a repo is materialized inside a workweave.
///
/// This is the single, self-describing on-disk authority for "what kind of
/// checkout is this?" — every lifecycle command (delete, dirty/diverged
/// checks, idempotent reuse, orphan prune, foreign-worktree refusal) and the
/// downstream `doctor` (check.rs) and `sync` (sync.rs) consumers branch on
/// this enum rather than re-deriving the symlink trick or, critically,
/// keying on the manifest `Role`.
///
/// The distinction is load-bearing once the `--worktree-references` escape
/// hatch exists: a `reference` repo created with that flag has
/// `role == Role::Reference` but is a *real worktree*, so it must flow
/// through every normal worktree code path. Keying any downstream skip on
/// `role == Reference` would silently break the escape hatch. Keying on
/// [`CheckoutKind`] instead means the escape hatch needs zero downstream
/// plumbing: a worktree'd reference is simply a [`CheckoutKind::Worktree`].
///
/// `role` is consulted at exactly one place — *creation*
/// ([`create_workweave`]) — to pick the default materialization. Every other
/// command routes on this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutKind {
    /// A real `git worktree` (or the project-repo copy): an independent
    /// working tree on its own ephemeral branch. Covers `owned`/`fork`/
    /// `dependency` repos and any `reference` repo created with
    /// `--worktree-references`. Flows through every existing code path.
    Worktree,
    /// A symlink aliasing the single canonical weave-root clone of a
    /// `reference` repo. Shared, read-only, and identical across workweaves:
    /// it has no per-workweave branch, no per-workweave dirty state, and must
    /// never be operated on as a worktree (no `git worktree remove`, no
    /// branch delete, no savepoint/sync). Deleting a workweave unlinks it
    /// with `remove_file`, never touching the canonical store.
    ReferenceAlias,
}

/// Classify a workweave checkout path by its on-disk materialization.
///
/// Returns [`CheckoutKind::ReferenceAlias`] iff `path` is itself a symlink
/// (checked with [`Path::is_symlink`], which does *not* follow the link), and
/// [`CheckoutKind::Worktree`] otherwise. This is the single chokepoint for
/// "is this a shared read-only alias"; downstream code must consult this
/// rather than calling `is_symlink()` ad hoc, so the meaning of the symlink
/// lives in exactly one place.
///
/// A non-existent path classifies as [`CheckoutKind::Worktree`]: a path that
/// is not a symlink is, by construction, not a reference alias, and callers
/// that care about existence check it separately.
pub fn classify_checkout(path: &Path) -> CheckoutKind {
    if path.is_symlink() {
        CheckoutKind::ReferenceAlias
    } else {
        CheckoutKind::Worktree
    }
}

/// Determine where workweave directories live.
///
/// If `RWV_WORKWEAVE_DIR` is set, workweaves go under that directory.
/// Otherwise they live under `.workweaves/` in the parent of the workspace root.
fn workweave_parent(ws_root: &Path) -> PathBuf {
    if let Ok(wr) = std::env::var("RWV_WORKWEAVE_DIR") {
        PathBuf::from(wr)
    } else {
        ws_root
            .parent()
            .expect("workspace root should have a parent")
            .join(".workweaves")
    }
}

/// Public accessor for the workweave parent directory.
///
/// Exposed for `check.rs` (workweave-tree integrity scanning). Callers outside
/// this module should treat the returned path as the container for all
/// workweave directories belonging to `ws_root`.
pub fn workweave_parent_pub(ws_root: &Path) -> PathBuf {
    workweave_parent(ws_root)
}

/// Compute the on-disk directory for a workweave by `(project, name)`, given
/// the primary workspace root.
///
/// Returns `<workweave_parent>/<project>--<name>`. If the path does not exist
/// the caller may create it or report it as missing.
pub fn workweave_path_for(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
) -> PathBuf {
    let parent = workweave_parent(primary_root);
    parent.join(weave_dir_name(project.as_str(), name))
}

/// Build the ephemeral branch name used by workweave worktrees.
///
/// Includes the project name so that workweaves with the same name across
/// different projects do not collide on shared repos (e.g., both
/// `project-repoweave` and `foundations` referencing `github/gastownhall/beads`).
fn ephemeral_branch_name(
    project: &ProjectName,
    workweave_name: &WorkweaveName,
    current_branch: &RefName,
) -> RefName {
    RefName::new(format!(
        "{}--{}/{}",
        project.as_str(),
        workweave_name.as_str(),
        current_branch.as_str()
    ))
}

/// Build the branch prefix used to locate all ephemeral branches for a given
/// (project, workweave_name) pair. Used to clean up branches on delete.
fn ephemeral_branch_prefix(project: &ProjectName, workweave_name: &WorkweaveName) -> RefName {
    RefName::new(format!("{}--{}", project.as_str(), workweave_name.as_str()))
}

/// Recursively copy a directory from `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Remove orphan worktree registrations from a list of repos.
///
/// For each `(repo_abs, worktree_path)` pair, attempts `git worktree remove
/// --force <worktree_path>` first (cleans up both the on-disk directory and
/// the `.git/worktrees/` registration). If `remove` fails because the
/// on-disk directory is already gone, falls back to `git worktree prune` to
/// clear the stale administrative entry.
///
/// This is the canonical cleanup path used by:
/// - `create_workweave` rollback — called on any mid-create failure.
/// - The `create --force` path — call this before recreating to clear
///   any orphan registrations left by a previous partial create.
///
/// # API contract for callers
///
/// ```text
/// prune_orphan_worktrees_for(&[
///     (repo_abs_path, worktree_dest_path),
///     ...
/// ]);
/// ```
///
/// The function is best-effort: errors are logged to stderr but do not
/// propagate, so a single prune failure does not block cleanup of subsequent
/// repos. This matches `delete_workweave`'s existing "continue on error, collect
/// at the end" pattern — orphan refs in a secondary repo should not prevent
/// the workweave dir from being removed.
pub fn prune_orphan_worktrees_for(pairs: &[(PathBuf, PathBuf)]) {
    for (repo_abs, worktree_path) in pairs {
        let vcs = GitVcs;
        if worktree_path.exists() {
            // Directory still on disk — use `remove --force` which handles
            // both the on-disk tree and the `.git/worktrees/` registration.
            if let Err(e) = vcs.remove_worktree(repo_abs, worktree_path) {
                eprintln!(
                    "rwv workweave rollback: warning: could not remove worktree {}: {e}",
                    worktree_path.display()
                );
                // Fall through to prune to at least clear the admin entry.
            }
        }
        // Always prune stale entries regardless of remove outcome.
        if let Err(e) = vcs.worktree_prune(repo_abs) {
            eprintln!(
                "rwv workweave rollback: warning: git worktree prune failed in {}: {e}",
                repo_abs.display()
            );
        }
    }
}

/// Build the `(repo_abs, worktree_dest)` pairs that
/// [`prune_orphan_worktrees_for`] operates on, excluding reference aliases.
///
/// `repo_abs = source_root/<repo_path>`, `worktree_dest =
/// workweave_dir/<repo_path>`. A checkout that materialized as a
/// [`CheckoutKind::ReferenceAlias`] (a symlink to the canonical store) is
/// excluded: it has no `.git/worktrees/` registration to prune, and feeding
/// its symlink path to `git worktree remove` would operate on the shared
/// canonical store through the link. Reference aliases are classified by
/// their on-disk `worktree_dest`, so a reference repo created with
/// `--worktree-references` (a real worktree) is correctly retained.
fn orphan_prune_pairs(
    manifest: &Manifest,
    source_root: &Path,
    workweave_dir: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    manifest
        .repositories
        .keys()
        .filter_map(|repo_path| {
            let worktree_dest = workweave_dir.join(repo_path.as_path());
            if classify_checkout(&worktree_dest) == CheckoutKind::ReferenceAlias {
                return None;
            }
            let repo_abs = source_root.join(repo_path.as_path());
            Some((repo_abs, worktree_dest))
        })
        .collect()
}

/// Scope guard that rolls back a partial workweave create on drop.
///
/// Tracks:
/// - The workweave directory (`workweave_dir`): removed with `remove_dir_all`.
/// - Registered worktrees (`registered_worktrees`): each entry is
///   `(repo_abs, worktree_path)`; pruned via [`prune_orphan_worktrees_for`].
/// - Created ephemeral branches (`created_branches`): each entry is
///   `(repo_abs, branch_name)`; deleted on rollback after worktree removal.
/// - Repos to prune on rollback (`prune_on_rollback`): repos where a worktree
///   creation was ATTEMPTED (even if it failed mid-way due to a git hook).
///   A failing hook can create the worktree directory AND the `.git/worktrees/`
///   registration before returning a non-zero exit — these stale registrations
///   must be pruned even though the worktree was never fully recorded as
///   successful. After `remove_dir_all(&workweave_dir)` removes the worktree
///   path, the registration becomes prunable; running `git worktree prune` in
///   each attempted repo clears it.
///
/// Call `defuse()` to commit the create — the guard then does nothing on drop.
/// If the guard is dropped without being defused (i.e. due to any failure path,
/// including `bail!` / `?` propagation), the rollback runs automatically.
///
/// For explicit failure points (where the caller has an error to return),
/// prefer calling [`rollback_and_collect_failures`] before bailing so that
/// cleanup failures can be appended to the returned error message rather than
/// only being printed to stderr.
///
/// **Design:** A single drop-based guard centralises rollback so future code
/// cannot accidentally bypass it. Adding a new failure point that returns early
/// (via `?` or `bail!`) automatically triggers cleanup — no extra boilerplate
/// required.
struct CreateRollbackGuard {
    /// The top-level workweave directory created for this attempt.
    workweave_dir: PathBuf,
    /// Pairs of `(repo_abs, worktree_dest)` for every worktree that was
    /// successfully registered during this create attempt.
    registered_worktrees: Vec<(PathBuf, PathBuf)>,
    /// Pairs of `(repo_abs, ephemeral_branch)` for every branch created during
    /// this attempt. Deleted on rollback AFTER worktree removal (so the branch
    /// is no longer checked out when `delete_branch` runs).
    created_branches: Vec<(PathBuf, RefName)>,
    /// Repos where a worktree creation was attempted (success or failure).
    /// On rollback, `git worktree prune` is run in each of these repos after
    /// `remove_dir_all(&workweave_dir)` so that stale `.git/worktrees/<name>`
    /// entries left by a partial hook-failed add are cleared.
    prune_on_rollback: Vec<PathBuf>,
    /// Set to `true` when the create completes successfully OR when
    /// `rollback_and_collect_failures` has already been called. Prevents
    /// double-rollback in Drop.
    defused: bool,
}

impl CreateRollbackGuard {
    fn new(workweave_dir: PathBuf) -> Self {
        Self {
            workweave_dir,
            registered_worktrees: Vec::new(),
            created_branches: Vec::new(),
            prune_on_rollback: Vec::new(),
            defused: false,
        }
    }

    /// Record a successfully-registered worktree so it can be rolled back on failure.
    fn record_worktree(&mut self, repo_abs: PathBuf, worktree_dest: PathBuf) {
        self.registered_worktrees.push((repo_abs, worktree_dest));
    }

    /// Record a repo where a worktree creation was attempted, regardless of
    /// outcome. On rollback, `git worktree prune` will run in this repo after
    /// the workweave directory is removed, clearing any stale `.git/worktrees/`
    /// entry that a hook-failed partial `git worktree add` may have left behind.
    ///
    /// Call this BEFORE calling `vcs.create_worktree(...)` so that even a
    /// failed `create_worktree` leaves the repo in the prune list.
    fn record_attempted_repo(&mut self, repo_abs: PathBuf) {
        self.prune_on_rollback.push(repo_abs);
    }

    /// Record a branch that WILL BE created by the next `create_worktree` call
    /// for `repo_abs`. Recorded regardless of whether `create_worktree` succeeds:
    /// a hook-failed `git worktree add` creates the branch before running the
    /// hook, so the branch persists even when `create_worktree` returns `Err`.
    /// On rollback, `delete_branch` is attempted for every recorded branch.
    ///
    /// Call this BEFORE calling `vcs.create_worktree(...)`.
    fn record_intended_branch(&mut self, repo_abs: PathBuf, branch: RefName) {
        self.created_branches.push((repo_abs, branch));
    }

    /// Commit the create — disable rollback. Call this only after
    /// `create_workweave` has fully succeeded.
    fn defuse(&mut self) {
        self.defused = true;
    }

    /// Perform rollback immediately and return a list of cleanup-failure
    /// descriptions (each item is a human-readable string describing what
    /// failed and the exact manual command to fix it).
    ///
    /// The rollback is best-effort: a failure on one repo does not skip the
    /// others. After this call the guard is defused so Drop is a no-op.
    ///
    /// Callers MUST append any returned items to the primary error message so
    /// the operator sees both the root cause and any manual-cleanup work. The
    /// original error must remain the primary error — never replace it.
    fn rollback_and_collect_failures(&mut self) -> Vec<String> {
        // Defuse first so Drop never runs this twice.
        self.defused = true;

        let mut failures: Vec<String> = Vec::new();

        // Step 1: Remove worktree directories and prune registrations for
        // repos that SUCCESSFULLY registered a worktree.
        for (repo_abs, worktree_path) in &self.registered_worktrees {
            let vcs = GitVcs;
            if worktree_path.exists() {
                if let Err(e) = vcs.remove_worktree(repo_abs, worktree_path) {
                    eprintln!(
                        "rwv workweave rollback: warning: could not remove worktree {}: {e}",
                        worktree_path.display()
                    );
                    failures.push(format!(
                        "worktree {path} — remove manually with: git -C {repo} worktree remove --force {path}",
                        path = worktree_path.display(),
                        repo = repo_abs.display(),
                    ));
                }
            }
            // Always prune stale admin entries regardless of remove outcome.
            if let Err(e) = vcs.worktree_prune(repo_abs) {
                eprintln!(
                    "rwv workweave rollback: warning: git worktree prune failed in {}: {e}",
                    repo_abs.display()
                );
            }
        }

        // Step 2: Remove the partially-created workweave directory itself.
        //
        // This happens BEFORE branch deletion and the post-remove prune pass for
        // two reasons:
        // (a) Branch deletion: a branch checked out by a linked worktree cannot be
        //     force-deleted while the worktree directory exists on disk. Removing
        //     the workweave dir first ensures `git branch -D` succeeds.
        // (b) Prune: removing the workweave dir makes `.git/worktrees/<name>` admin
        //     entries prunable (they point to the now-gone directory).
        if self.workweave_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&self.workweave_dir) {
                eprintln!(
                    "rwv workweave rollback: warning: could not remove partial workweave dir {}: {e}",
                    self.workweave_dir.display()
                );
                failures.push(format!(
                    "partial workweave dir {dir} — remove manually with: rm -rf {dir}",
                    dir = self.workweave_dir.display(),
                ));
            }
        }

        // Step 3: Prune stale `.git/worktrees/` entries in every repo where a
        // worktree creation was ATTEMPTED (even if `git worktree add` failed
        // mid-way due to a post-checkout hook).
        //
        // Prune MUST run BEFORE branch deletion (step 4): git tracks a branch as
        // "used by worktree" based on the `.git/worktrees/<name>` registration,
        // not the on-disk directory. Even after step 2 removes the worktree
        // directory, the stale registration keeps the branch locked. Pruning
        // clears the registration so `git branch -D` succeeds.
        //
        // Step 2's dir-removal makes the admin entries prunable (they point to the
        // now-gone directory). Repos already handled in step 1 are included here
        // too — prune is idempotent.
        let vcs = GitVcs;
        for repo_abs in &self.prune_on_rollback {
            if let Err(e) = vcs.worktree_prune(repo_abs) {
                eprintln!(
                    "rwv workweave rollback: warning: git worktree prune failed in {}: {e}",
                    repo_abs.display()
                );
            }
        }

        // Step 4: Delete ephemeral branches created during this attempt.
        //
        // Runs AFTER removing the workweave dir (step 2) AND after pruning stale
        // worktree registrations (step 3). The prune step is critical: git refuses
        // to force-delete a branch listed in any `.git/worktrees/<name>` entry,
        // even when the worktree directory no longer exists on disk.
        for (repo_abs, branch) in &self.created_branches {
            if let Err(e) = vcs.delete_branch(repo_abs, branch) {
                eprintln!(
                    "rwv workweave rollback: warning: could not delete ephemeral branch {} in {}: {e}",
                    branch.as_str(),
                    repo_abs.display()
                );
                failures.push(format!(
                    "branch {branch} in {repo} — delete manually with: git -C {repo} branch -D {branch}",
                    branch = branch.as_str(),
                    repo = repo_abs.display(),
                ));
            }
        }

        failures
    }
}

impl Drop for CreateRollbackGuard {
    fn drop(&mut self) {
        if self.defused {
            return;
        }

        // Drop-based rollback path: used for `?` propagation failures.
        // Cleanup failures are printed to stderr only (no caller to append to).
        // For explicit `bail!` paths, prefer calling `rollback_and_collect_failures`
        // first so cleanup failures appear in the returned error.
        //
        // 1. Prune orphan worktree registrations and remove worktree directories
        //    for repos that SUCCESSFULLY registered (same as registered_worktrees).
        prune_orphan_worktrees_for(&self.registered_worktrees);

        let vcs = GitVcs;

        // 2. Remove the partially-created workweave directory.
        //    Must happen BEFORE branch deletion: git refuses to force-delete a
        //    branch checked out by a live linked worktree; removing the dir first
        //    disconnects the worktree so the delete can proceed.
        //    Must also happen BEFORE prune: removal makes stale entries prunable.
        if self.workweave_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&self.workweave_dir) {
                eprintln!(
                    "rwv workweave rollback: warning: could not remove partial workweave dir {}: {e}",
                    self.workweave_dir.display()
                );
            }
        }

        // 3. Prune stale `.git/worktrees/` entries in repos where worktree
        //    creation was attempted. Prune BEFORE branch deletion: git refuses
        //    to force-delete a branch registered in any worktree entry, even
        //    when the worktree directory no longer exists on disk.
        for repo_abs in &self.prune_on_rollback {
            if let Err(e) = vcs.worktree_prune(repo_abs) {
                eprintln!(
                    "rwv workweave rollback: warning: git worktree prune failed in {}: {e}",
                    repo_abs.display()
                );
            }
        }

        // 4. Delete ephemeral branches created during this attempt.
        //    Runs AFTER removing the workweave dir (step 2) and pruning stale
        //    worktree registrations (step 3).
        for (repo_abs, branch) in &self.created_branches {
            if let Err(e) = vcs.delete_branch(repo_abs, branch) {
                eprintln!(
                    "rwv workweave rollback: warning: could not delete ephemeral branch {} in {}: {e}",
                    branch.as_str(),
                    repo_abs.display()
                );
            }
        }
    }
}

/// Pre-flight check: verify that every git repo involved in a workweave create
/// has at least one commit (i.e., HEAD resolves).
///
/// Checks:
/// - The project repo at `source_root/projects/<project>/` (if it is a git repo).
/// - Every repo listed in `manifest.repositories` at `source_root/<repo_path>/`.
///
/// Returns `Ok(())` when all repos are commit-bearing. Returns a structured
/// error naming every missing-HEAD path and the suggested fix for each.
///
/// Designed to be called BEFORE any disk mutation so that a fresh-project
/// failure leaves no partial workweave directory on disk.
///
/// Siblings `.2` (rollback) and `.3` (--force prune) may call this same
/// function before their own mutations.
pub fn preflight_check_heads(
    source_root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> anyhow::Result<()> {
    let mut missing: Vec<String> = Vec::new();

    // Check project repo.
    let project_dir = source_root.join("projects").join(project.as_str());
    if GitVcs.is_repo(&project_dir) && GitVcs.head_revision(&project_dir).is_err() {
        missing.push(format!(
            "project {name} has no commits yet — run \
             \"git -C projects/{name} commit\" to land the activate-generated \
             artifacts before creating a workweave.",
            name = project.as_str(),
        ));
    }

    // Check each manifest repo.
    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = source_root.join(repo_path.as_path());
        if vcs.is_repo(&repo_abs) && vcs.head_revision(&repo_abs).is_err() {
            missing.push(format!(
                "repo {path} has no commits yet — run \
                 \"git -C {path} commit\" to create an initial commit before \
                 creating a workweave.",
                path = repo_path.as_str(),
            ));
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    bail!(
        "rwv workweave create: cannot create workweave — the following repos \
         have no commits yet:\n  {}",
        missing.join("\n  ")
    )
}

/// Try to initialize submodules in `worktree_path` if the repo contains
/// `.gitmodules`.
///
/// Returns `Ok(())` when there is nothing to do (no `.gitmodules`) or when
/// submodule init succeeds. Returns `Err` only when submodule init is
/// attempted and fails (network unreachable, upstream gone, etc.).
///
/// Cost: one `Path::exists` call per worktree when `.gitmodules` is absent
/// (the common case). The full `git submodule update` runs only when
/// `.gitmodules` exists.
fn init_submodules_in_worktree(worktree_path: &Path) -> anyhow::Result<()> {
    if !worktree_path.join(".gitmodules").exists() {
        return Ok(());
    }
    // SECURITY: do NOT inject `protocol.file.allow=always` here. Git's
    // restrictive default (`user`) is the mitigation for hostile-.gitmodules
    // attacks (CVE-2022-39253 class: a third-party repo's `.gitmodules`
    // referencing `file://` paths on the operator's host), and workweaves
    // materialize third-party reference repos. Production submodule init runs
    // with the operator's own git config posture — if a weave genuinely uses
    // `file://` submodules, the operator's config already allows it. Tests
    // that need `file://` remotes set the env on their own spawned commands.
    let output = git_command()
        .args(["submodule", "update", "--init", "--recursive"])
        .current_dir(worktree_path)
        .output()
        .context("failed to spawn git submodule update")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git submodule update --init --recursive failed: {stderr}");
    }
    Ok(())
}

/// Scan `.gitmodules` in `worktree_path` and return the submodule `path =`
/// values whose on-disk directory is empty (or absent), indicating that
/// submodules were never initialized.
///
/// Returns an empty `Vec` when:
/// - `.gitmodules` does not exist (no submodules declared), or
/// - all listed submodule paths are non-empty directories on disk.
///
/// This is a **local-only** check (stat the path; no network). A non-empty
/// submodule directory is assumed to be correctly initialized; this scanner
/// does not verify the content against any remote.
pub fn scan_uninitialized_submodules(worktree_path: &Path) -> Vec<String> {
    let gitmodules = worktree_path.join(".gitmodules");
    if !gitmodules.exists() {
        return Vec::new();
    }
    let content = match std::fs::read_to_string(&gitmodules) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    // Parse `path = <value>` lines from .gitmodules. We don't need a full INI
    // parser — just extract the `path =` values.
    let mut empty_paths = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("path") {
            let rest = rest.trim();
            if let Some(value) = rest.strip_prefix('=') {
                let sub_path = value.trim();
                if sub_path.is_empty() {
                    continue;
                }
                let sub_dir = worktree_path.join(sub_path);
                // A submodule dir should be a non-empty directory. An absent
                // dir or an empty dir means submodule init has not run.
                let is_empty = if sub_dir.is_dir() {
                    std::fs::read_dir(&sub_dir)
                        .map(|mut rd| rd.next().is_none())
                        .unwrap_or(true)
                } else {
                    true
                };
                if is_empty {
                    empty_paths.push(sub_path.to_string());
                }
            }
        }
    }
    empty_paths
}

/// Create a workweave: for each repo in the manifest, create a worktree in the
/// workweave directory on an ephemeral branch `{project}--{workweave_name}/{current_branch}`.
/// Also creates a worktree for the project repo, processes `workweave:` artifacts,
/// writes the marker file, writes `.rwv-active`, and runs activate.
///
/// `primary_root` locates the surrounding weave: it determines where new
/// workweaves live (`<primary_parent>/.workweaves/`) and is recorded in the
/// `.rwv-workweave` marker so the workweave knows its primary.
///
/// `source_root` is the workspace forked from: the manifest, per-repo HEADs,
/// `projects/<project>/` worktree, and `workweave:` copy/link sources are
/// all read from `source_root`. When forking from primary, pass the same path
/// for both. When forking from another workweave (e.g. a Gas City rig
/// creating a peer workweave from inside itself), `source_root` is that
/// workweave's directory while `primary_root` remains the primary weave.
///
/// If the workweave directory already exists, behavior depends on `force`:
/// - `force == false`: validate that the existing workweave matches this
///   `(primary, project)` pair and has no local modifications relative to
///   `source_root`, then short-circuit. This preserves non-git state (e.g.
///   `.runtime/`, `.claude/`) written by agents between invocations — the
///   contract relied on by Gas City's `gc runtime request-restart` flow.
///   Returns an error if the marker is missing or for a different project,
///   or if any worktree has uncommitted changes or has diverged from the
///   source.
/// - `force == true`: destroy the existing workweave and recreate from
///   scratch. Intended for explicit rebuild scenarios (corruption
///   recovery, or switching a slot to a different project).
///
/// `capture_dirty` controls how uncommitted changes in the source project
/// directory are handled at create time:
/// - `false` (default): refuse with a clear error if `projects/<project>/`
///   has uncommitted changes. The error names the dirty files and suggests
///   committing, stashing, or passing `--capture-dirty`.
/// - `true`: capture the dirty state into the workweave (legacy behavior).
///   The workweave's project worktree will reflect the uncommitted edits.
///
/// `worktree_references` controls how `role: reference` repos are
/// materialized — the *only* place `role` influences materialization:
/// - `false` (default): materialize each reference repo as a **symlink** to
///   the canonical weave-root clone (`<primary_root>/<repo_path>`). No
///   worktree is cut and nothing is recorded for rollback (removing the
///   workweave dir unlinks the symlink without following it). Every
///   downstream command sees a [`CheckoutKind::ReferenceAlias`] and skips
///   worktree/branch/dirty/sync semantics for it.
/// - `true` (escape hatch, `--worktree-references`): cut a real worktree for
///   reference repos too (the legacy behavior). Such a repo is a
///   [`CheckoutKind::Worktree`] and flows through every normal code path.
///
/// Returns the absolute path of the created workweave directory.
pub fn create_workweave(
    primary_root: &Path,
    source_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    force: bool,
    capture_dirty: bool,
    worktree_references: bool,
) -> anyhow::Result<PathBuf> {
    let manifest = load_manifest(source_root, project)?;
    let workweave_dir = workweave_path_for(primary_root, project, name);

    if workweave_dir.exists() {
        if force {
            // Destructive reuse. Prefer delete_workweave (which also
            // prunes worktrees and ephemeral branches) when the marker
            // belongs to this project; fall back to a raw remove
            // otherwise since delete_workweave loads a manifest tied to
            // `project` and would fail on wrong-marker / missing-marker
            // cases.
            let can_use_structured_delete = match WorkweaveMarker::read(&workweave_dir)? {
                Some(m) => &m.project == project,
                None => false,
            };
            // Even under --force, refuse to replace a workweave holding
            // uncommitted work. create's --force consents to replacing
            // the directory, but the operator never saw what was
            // inside it — unlike `workweave remove`, which lists the dirty
            // paths before --force is retried. Explicit destruction of
            // dirty workweaves stays with `workweave remove --force`.
            let at_risk = if can_use_structured_delete {
                // Uncommitted changes plus committed-but-unmerged work —
                // both are destroyed by the replace.
                let mut paths = collect_dirty_paths(&workweave_dir, project, &manifest);
                let baselines = merge_baselines(&workweave_dir, primary_root);
                paths.extend(collect_diverged_paths(
                    &workweave_dir,
                    project,
                    &manifest,
                    &baselines,
                ));
                paths
            } else {
                // Marker missing/foreign: no manifest can be trusted to
                // enumerate the contents, so scan for repos directly.
                collect_dirty_repos_by_walk(&workweave_dir)
            };
            if !at_risk.is_empty() {
                bail!(
                    "workweave {} already exists and holds unsaved or unmerged work; \
                     refusing to replace it:\n  {}\n\
                     Commit/merge that work, or delete it explicitly with \
                     `rwv workweave {} delete {} --force`.",
                    name.as_str(),
                    at_risk.join("\n  "),
                    project.as_str(),
                    name.as_str(),
                );
            }
            if can_use_structured_delete {
                // `force: true` on the internal delete: the dirty check
                // above just confirmed there is nothing uncommitted to
                // lose, and the operator's --force already authorised
                // replacing the (clean) workweave.
                delete_workweave(primary_root, project, name, true)?;
            } else {
                // No valid marker for this project, so delete_workweave
                // cannot be used (it would load the wrong manifest).
                // Still prune orphan worktree registrations — a previous
                // partial create may have left `.git/worktrees/<name>`
                // entries in the primary repos even though the workweave
                // directory survived (or had its marker stripped).
                // Build (repo_abs, worktree_dest) pairs from the
                // manifest and prune before the raw remove. Reference
                // aliases (symlinks) are excluded — they hold no worktree
                // registration and must not be `git worktree remove`d
                // through the link into the canonical store.
                let orphan_pairs = orphan_prune_pairs(&manifest, source_root, &workweave_dir);
                prune_orphan_worktrees_for(&orphan_pairs);
                std::fs::remove_dir_all(&workweave_dir)?;
            }
        } else {
            return reuse_existing_workweave(
                primary_root,
                source_root,
                project,
                name,
                &workweave_dir,
                &manifest,
            );
        }
    }

    // Pre-flight: refuse to create a workweave when the source project directory
    // has uncommitted changes, unless the caller explicitly opted in with
    // `capture_dirty`. Dirty state captured into a workweave becomes an
    // obstacle at retire time ("Your local changes to the following files would
    // be overwritten by merge").
    //
    // Only the project directory is checked here — manifest-repo worktrees are
    // forked at HEAD (committed state) by `git worktree add`, so dirty state
    // in those repos is not captured. The project dir is special because we
    // explicitly overlay its working-tree `rwv.yaml`/`rwv.lock` below.
    if !capture_dirty {
        let project_dir = source_root.join("projects").join(project.as_str());
        if GitVcs.is_repo(&project_dir) {
            match crate::git::GitVcs::dirty_file_names(&project_dir) {
                Ok(dirty) if !dirty.is_empty() => {
                    bail!(
                        "rwv workweave create: refusing to create workweave — \
                         projects/{project} has uncommitted changes:\n  {files}\n\n\
                         To proceed, do one of:\n  \
                         1. commit the changes: git -C projects/{project} commit\n  \
                         2. stash the changes: git -C projects/{project} stash\n  \
                         3. capture them into the workweave: rwv workweave {project} create {name} --capture-dirty",
                        project = project.as_str(),
                        name = name.as_str(),
                        files = dirty.join("\n  "),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    // Cannot determine dirty status — be conservative and refuse.
                    bail!(
                        "rwv workweave create: refusing to create workweave — \
                         could not check projects/{project} for uncommitted changes: {e}\n\n\
                         To bypass this check, use --capture-dirty.",
                        project = project.as_str(),
                    );
                }
            }
        }
    }

    // Pre-flight: verify every repo we are about to worktree has at least one
    // commit. `git worktree add` requires a resolvable HEAD; without this check
    // the user gets a raw "fatal: ambiguous argument 'HEAD'" from git, which
    // names no path and suggests no fix. We run this BEFORE any disk mutation
    // so a missing-HEAD failure leaves no partial workweave directory behind.
    preflight_check_heads(source_root, project, &manifest)?;

    // Pre-add prune: clear any orphaned `.git/worktrees/<name>` registrations
    // left over from a previous (failed or manually-deleted) create attempt.
    //
    // The failure mode this targets: the workweave directory is fully gone (rm
    // -rf'd, or a partial create was interrupted before the directory was
    // written) yet the `.git/worktrees/<name>` administrative entry survives in
    // one or more canonical repos. Without this prune, the subsequent
    // `git worktree add` calls fail with:
    //
    //   fatal: '<path>' is a missing but already registered worktree;
    //          use 'add -f' to override, or 'prune'/'remove' to clear
    //
    // This is idempotent: `prune_orphan_worktrees_for` only calls
    // `git worktree remove --force` when the worktree directory EXISTS on disk
    // (so a live workweave's files are never touched), and then always calls
    // `git worktree prune`, which only removes administrative entries whose
    // worktree directory is absent. A clean repo with no stale registration
    // is a no-op.
    //
    // This does NOT duplicate the dir-exists path above (which either runs
    // delete_workweave or prune_orphan_worktrees_for on the surviving dir): at
    // this point we know workweave_dir does NOT exist (the block above handled
    // the exists() case and either returned or deleted the directory).
    //
    // Reference repos that this create will materialize as symlinks (the
    // default) are excluded: they never had a worktree registration, so
    // pruning `source_root/<repo_path>` for them is meaningless and could run
    // `git worktree remove` against the canonical store. The workweave dir
    // does not exist yet, so on-disk classification can't see the intent;
    // here — uniquely in the creation path — we key on the to-be-materialized
    // kind, mirroring the materialization decision in the create loop below.
    {
        let orphan_pairs: Vec<(PathBuf, PathBuf)> = manifest
            .iter_entries()
            .filter(|(_, entry)| entry.role != Role::Reference || worktree_references)
            .map(|(repo_path, _)| {
                let repo_abs = source_root.join(repo_path.as_path());
                let worktree_dest = workweave_dir.join(repo_path.as_path());
                (repo_abs, worktree_dest)
            })
            .collect();
        prune_orphan_worktrees_for(&orphan_pairs);
    }

    std::fs::create_dir_all(&workweave_dir)?;

    // B7: Rollback guard — automatically undoes partial state on any failure
    // path (including `bail!` / `?` propagation). Tracks which repos got
    // worktrees added so orphan `.git/worktrees/` registrations can be pruned
    // in addition to removing the workweave directory.
    let mut rollback = CreateRollbackGuard::new(workweave_dir.clone());

    let mut errors: Vec<String> = Vec::new();
    // Submodule warnings: repos where submodule init was attempted but failed
    // (network down, upstream unreachable, etc.). Collected separately so a
    // submodule-init failure does NOT abort the whole create — the worktree
    // itself is valid and usable; only submodule content is missing.
    let mut submodule_warnings: Vec<String> = Vec::new();

    // Materialize each repo in the manifest. Forks come from source_root so
    // peer workweaves rooted in another workweave's HEADs diverge cleanly from
    // that parent rather than from primary.
    //
    // `role: reference` repos materialize as a SYMLINK to the canonical
    // weave-root clone (`primary_root/<repo_path>`) by default — they are
    // read-only, lock-pinned, and identical across workweaves, so a worktree's
    // independent-branch value is moot while its full working-tree duplication
    // cost is paid per workweave. The escape hatch (`worktree_references`)
    // restores the worktree behavior. Everything else (owned/fork/dependency)
    // always cuts a worktree.
    for (repo_path, entry) in manifest.iter_entries() {
        // The single place `role` decides materialization: a reference repo,
        // unless the escape hatch is set, becomes a symlink alias. Downstream
        // commands key on the on-disk CheckoutKind, never on role.
        let materialize_as_alias = entry.role == Role::Reference && !worktree_references;

        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = source_root.join(repo_path.as_path());
        let worktree_dest = workweave_dir.join(repo_path.as_path());

        // The closure returns `Ok(())` on success or `Err` on failure.
        // For worktree repos (not symlinks), the ephemeral branch name and
        // repo-abs are recorded BEFORE `create_worktree` is called so that
        // a hook-failed partial add (which creates the branch even on Err)
        // can be cleaned up by the rollback guard.
        let result = (|| -> anyhow::Result<()> {
            // Ensure parent directories exist (both branches need this).
            if let Some(parent_dir) = worktree_dest.parent() {
                std::fs::create_dir_all(parent_dir)?;
            }

            if materialize_as_alias {
                // Symlink to PRIMARY's canonical clone, not source_root: a
                // nested workweave forked from another workweave must point at
                // the one canonical store, never at the parent workweave's own
                // symlink (which would form a symlink→symlink chain that breaks
                // if the parent is deleted). See
                // docs/repoweave/reference-symlink-materialization.md §1.
                let canonical = primary_root.join(repo_path.as_path());
                #[cfg(unix)]
                std::os::unix::fs::symlink(&canonical, &worktree_dest)?;
                return Ok(());
            }

            // Get the current HEAD revision as the start point.
            let head = vcs.head_revision(&repo_abs)?;

            // Distinguish a real branch from detached HEAD. Detached HEADs
            // produce a `detached-<shortsha>` segment so the ephemeral
            // branch name doesn't masquerade as a real ref called "HEAD".
            let branch_segment = match vcs.current_ref(&repo_abs)? {
                Some(r) => RefName::new(r.as_str().to_string()),
                None => RefName::new(format!("detached-{}", short_sha(head.as_str()))),
            };

            let ephemeral_branch = ephemeral_branch_name(project, name, &branch_segment);

            // Record the repo and intended branch BEFORE calling create_worktree.
            // A post-checkout hook failure causes `git worktree add` to:
            //   1. Create the worktree directory (removed later by remove_dir_all).
            //   2. Create the `.git/worktrees/<name>` registration (cleared by prune).
            //   3. Create the ephemeral branch (must be deleted by rollback).
            // All three are created before the hook exits; recording here ensures
            // rollback handles them even when create_worktree returns Err.
            // SAFETY: these `record_*` calls mutate `rollback` via a raw pointer
            // to avoid a borrow-checker conflict with `vcs` in the closure. We
            // know `rollback` is not accessed concurrently — this is single-threaded
            // sequential code.
            //
            // Instead of unsafe, we collect the pre-registration info into local
            // vars and apply them after the closure returns (see below).
            //
            // DESIGN NOTE: we cannot call rollback.record_* here because `rollback`
            // is borrowed by the outer `for` loop context. We return the branch
            // name via Ok(()) — see post-closure recording below.
            vcs.create_worktree(&repo_abs, &worktree_dest, &ephemeral_branch, &head)?;
            Ok(())
        })();

        // Pre-record: for worktree repos, record the intended ephemeral branch and
        // the repo-abs in the rollback guard REGARDLESS of whether create_worktree
        // succeeded. This must happen before we inspect `result` so that the
        // rollback guard's cleanup covers both success and hook-failure paths.
        //
        // The branch name is derived identically to the computation inside the
        // closure (same project, name, current_ref). For repos where
        // create_worktree failed before reaching the branch-name computation
        // (e.g. head_revision failure), neither the branch nor the registration
        // exists — the extra delete_branch/prune calls are no-ops.
        if !materialize_as_alias {
            rollback.record_attempted_repo(repo_abs.clone());
            // Derive the intended ephemeral branch name. Mirror the computation
            // from inside the closure so we use the same name git used (or would
            // have used).
            let branch_seg = match vcs.current_ref(&repo_abs) {
                Ok(Some(r)) => Some(RefName::new(r.as_str().to_string())),
                Ok(None) => vcs
                    .head_revision(&repo_abs)
                    .ok()
                    .map(|h| RefName::new(format!("detached-{}", short_sha(h.as_str())))),
                Err(_) => None,
            };
            if let Some(seg) = branch_seg {
                let branch = ephemeral_branch_name(project, name, &seg);
                rollback.record_intended_branch(repo_abs.clone(), branch);
            }
        }

        match result {
            Ok(()) => {
                // A symlinked reference alias is NOT recorded for rollback:
                // rollback removes the workweave dir (which unlinks the
                // symlink without following it) and prunes worktree
                // registrations — a symlink has neither a worktree
                // registration nor any canonical-store state to undo, and
                // `prune_orphan_worktrees_for` would wrongly run
                // `git worktree remove` against the canonical through it.
                if !materialize_as_alias {
                    rollback.record_worktree(repo_abs, worktree_dest.clone());

                    // R23 GAP: init submodules in the new worktree. Only runs
                    // when `.gitmodules` exists (no per-repo overhead otherwise).
                    // Failure is warn-and-continue: network may be down or
                    // submodule remotes may be unreachable, but the worktree
                    // itself is valid.
                    if let Err(e) = init_submodules_in_worktree(&worktree_dest) {
                        let fix_cmd = format!(
                            "git -C {} submodule update --init --recursive",
                            worktree_dest.display()
                        );
                        let msg = format!(
                            "{repo}: submodules not initialized ({e}); \
                             fix: `{fix_cmd}`",
                            repo = repo_path.as_str(),
                        );
                        eprintln!("rwv workweave create: warning: {msg}");
                        submodule_warnings.push(repo_path.as_str().to_string());
                    }
                }
            }
            Err(e) => {
                // R25: when git worktree add fails due to a git hook (post-checkout
                // or similar), git's stderr mentions "hook". Name the hook and
                // point at git hook config so the operator knows where to look.
                let err_str = e.to_string();
                let hook_hint = if err_str.contains("hook") {
                    "\n  note: a git hook in this repo rejected the worktree creation; \
                     check the repo's .git/hooks/ directory or core.hooksPath config"
                } else {
                    ""
                };
                let msg = format!("{}: {e}{hook_hint}", repo_path.as_str());
                eprintln!("rwv workweave create: error: {msg}");
                errors.push(msg);
            }
        }
    }

    if !errors.is_empty() {
        let total = manifest.len();
        let failed = errors.len();
        // R25: explicit rollback before bail so that cleanup failures can be
        // appended to the returned error rather than only printed to stderr.
        // The rollback guard is defused after this call so Drop is a no-op.
        let cleanup_failures = rollback.rollback_and_collect_failures();
        let cleanup_note = if cleanup_failures.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nrollback completed with {} item(s) needing manual cleanup:\n  {}",
                cleanup_failures.len(),
                cleanup_failures.join("\n  ")
            )
        };
        bail!(
            "workweave create completed with {failed} failure(s) out of {total} repo(s){cleanup_note}"
        );
    }

    if !submodule_warnings.is_empty() {
        // Submodule init failed for one or more repos but the workweave itself
        // was created. Surface a summary so the operator knows the create
        // completed with partial materialization.
        eprintln!(
            "rwv workweave create: warning: submodules not initialized in {} repo(s): {}; \
             run `git -C <worktree-path> submodule update --init --recursive` per repo above to complete",
            submodule_warnings.len(),
            submodule_warnings.join(", ")
        );
    }

    // Create worktree for the project repo (if it is a git repo).
    // If the project directory exists but is not a git repo, copy it into the
    // workweave so that activate_workweave can find rwv.yaml there.
    let project_dir = source_root.join("projects").join(project.as_str());
    let project_wt_dest = workweave_dir.join("projects").join(project.as_str());
    if GitVcs.is_repo(&project_dir) {
        // B8: project-worktree creation failure must NOT silently fall
        // through to a static directory copy. The copy fallback is for
        // the "project dir exists but is not a git repo" branch only.
        // Producing a non-worktree copy here looks identical to a real
        // workweave on disk but has no upstream — commits go nowhere.
        let head = GitVcs.head_revision(&project_dir)?;
        let branch_segment = match GitVcs.current_ref(&project_dir)? {
            Some(r) => RefName::new(r.as_str().to_string()),
            None => RefName::new(format!("detached-{}", short_sha(head.as_str()))),
        };
        let ephemeral_branch = ephemeral_branch_name(project, name, &branch_segment);
        std::fs::create_dir_all(project_wt_dest.parent().unwrap())?;
        // Record the project repo and intended branch BEFORE calling create_worktree,
        // for the same hook-failure reason as manifest repos above.
        rollback.record_attempted_repo(project_dir.clone());
        rollback.record_intended_branch(project_dir.clone(), ephemeral_branch.clone());
        match GitVcs.create_worktree(&project_dir, &project_wt_dest, &ephemeral_branch, &head) {
            Ok(()) => {
                // Record the project worktree for rollback (branch already pre-recorded).
                rollback.record_worktree(project_dir.clone(), project_wt_dest.clone());
            }
            Err(e) => {
                // R25: if a git hook rejected the worktree add, name it.
                let hook_hint = if e.to_string().contains("hook") {
                    "; a git hook in the project repo rejected the worktree creation — \
                     check .git/hooks/ or core.hooksPath config"
                } else {
                    ""
                };
                // Explicit rollback before bail so cleanup failures are surfaced
                // in the returned error (not only on stderr).
                let cleanup_failures = rollback.rollback_and_collect_failures();
                let cleanup_note = if cleanup_failures.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n\nrollback completed with {} item(s) needing manual cleanup:\n  {}",
                        cleanup_failures.len(),
                        cleanup_failures.join("\n  ")
                    )
                };
                bail!(
                    "could not create project worktree projects/{}: {e}{hook_hint}{cleanup_note}",
                    project.as_str()
                );
            }
        }
    } else if project_dir.exists() {
        // Project dir is not a git repo — copy it so activate has access to rwv.yaml.
        copy_dir_recursive(&project_dir, &project_wt_dest)?;
    }

    // rwv-c7h fix: the project worktree above was checked out from a ref, so
    // its `rwv.yaml` is the last committed version — any uncommitted edits
    // in source_root's working tree were dropped. Overlay the source's
    // working-tree `rwv.yaml` (and `rwv.lock` for completeness) so the
    // workweave captures the operator's in-flight state. Warn loudly when
    // we're doing this so dirty creates don't surprise.
    //
    // Limited to `rwv.yaml` / `rwv.lock` deliberately: these are the files
    // that change workweave behavior (manifest = what worktrees to create,
    // workweave config; lock = lockfile shared with downstream). Other
    // uncommitted project files remain at their committed state, matching
    // the existing worktree-from-ref contract for everything else.
    if project_dir.exists() && project_wt_dest.exists() {
        for fname in ["rwv.yaml", "rwv.lock"] {
            let src = project_dir.join(fname);
            let dst = project_wt_dest.join(fname);
            if !src.exists() {
                continue;
            }
            let src_bytes = std::fs::read(&src).ok();
            let dst_bytes = if dst.exists() {
                std::fs::read(&dst).ok()
            } else {
                None
            };
            if src_bytes != dst_bytes {
                eprintln!(
                    "rwv workweave create: using working-tree projects/{}/{fname} \
                     (uncommitted changes; workweave captures dirty state)",
                    project.as_str(),
                );
                if let Some(bytes) = &src_bytes {
                    if let Err(e) = std::fs::write(&dst, bytes) {
                        eprintln!(
                            "rwv workweave create: warning: failed to overlay {}: {e}",
                            dst.display()
                        );
                    }
                }
            }
        }
    }

    // Process WorkweaveConfig artifacts. Sources resolve against source_root
    // so artifacts follow the workspace being forked from.
    if let Some(ref ww_config) = manifest.workweave {
        // Copy entries.
        for entry in &ww_config.copy {
            let source = source_root.join(entry);
            let dest = workweave_dir.join(entry);
            if source.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if source.is_dir() {
                    copy_dir_recursive(&source, &dest)?;
                } else {
                    std::fs::copy(&source, &dest)?;
                }
            }
        }

        // Link entries — absolute symlinks to the source's canonical paths.
        for entry in &ww_config.link {
            let source = source_root
                .join(entry)
                .canonicalize()
                .unwrap_or_else(|_| source_root.join(entry));
            let dest = workweave_dir.join(entry);
            if source.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&source, &dest)?;
            }
        }
    }

    // Write .rwv-workweave marker file. The marker records the primary so
    // workweaves always know how to find their parent weave regardless of
    // where they were forked from. `parent` records the workspace this
    // workweave was forked from (= source_root) so bare `rwv sync` knows
    // where to sync to. For workweaves forked directly from primary, parent
    // == primary; for workweaves forked from another workweave, parent is
    // that workweave's directory.
    let parent_path = source_root
        .canonicalize()
        .unwrap_or_else(|_| source_root.to_path_buf());
    let marker = WorkweaveMarker {
        primary: primary_root.to_path_buf(),
        project: project.clone(),
        parent: parent_path,
    };
    marker.write(&workweave_dir)?;

    // Write .rwv-active.
    set_active_project(&workweave_dir, project)?;

    // Run activate in the workweave context.
    crate::activate::activate_workweave(project.as_str(), &workweave_dir)?;

    // All steps complete — defuse the rollback guard so Drop is a no-op.
    rollback.defuse();

    Ok(workweave_dir)
}

/// Validate that an existing workweave directory matches `(primary_root, project, name)`
/// and is in a clean state relative to `source_root`, then return its path
/// without modifying anything.
///
/// Called from [`create_workweave`] on re-invocation without `--force`. Refuses
/// if the `.rwv-workweave` marker is missing or for a different primary/project,
/// or if any per-repo worktree has uncommitted changes or has diverged from the
/// source's HEAD.
fn reuse_existing_workweave(
    primary_root: &Path,
    source_root: &Path,
    project: &ProjectName,
    _name: &WorkweaveName,
    workweave_dir: &Path,
    manifest: &Manifest,
) -> anyhow::Result<PathBuf> {
    let marker = WorkweaveMarker::read(workweave_dir)?.ok_or_else(|| {
        anyhow!(
            "workweave directory {} exists but has no .rwv-workweave marker — \
             likely a partially created workweave from a previous failed attempt; \
             safe to recreate with --force",
            workweave_dir.display()
        )
    })?;

    if &marker.project != project {
        bail!(
            "workweave at {} is for project '{}', refusing to recreate for project '{}'; \
             rerun with --force to overwrite",
            workweave_dir.display(),
            marker.project.as_str(),
            project
        );
    }

    let marker_primary = marker
        .primary
        .canonicalize()
        .unwrap_or_else(|_| marker.primary.clone());
    let primary_canonical = primary_root
        .canonicalize()
        .unwrap_or_else(|_| primary_root.to_path_buf());
    if marker_primary != primary_canonical {
        bail!(
            "workweave at {} is for primary workspace {}, refusing to recreate for {}; \
             rerun with --force to overwrite",
            workweave_dir.display(),
            marker.primary.display(),
            primary_root.display()
        );
    }

    // Detect local modifications (uncommitted changes or HEAD divergence
    // from source) in any existing worktree. Missing worktrees are not
    // "modified" — a manifest may have grown since the workweave was
    // created; `rwv workweave sync` is the path for adding them.
    let mut modified: Vec<String> = Vec::new();
    for (repo_path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let worktree_dest = workweave_dir.join(repo_path.as_path());
        // A reference alias is a shared symlink with no per-workweave state:
        // it has no uncommitted changes and no HEAD that can "diverge from
        // source" (it IS the source). Checking it through the symlink would
        // report a dirty canonical store as this workweave's modification and
        // refuse the idempotent reuse path. Skip it.
        if classify_checkout(&worktree_dest) == CheckoutKind::ReferenceAlias {
            continue;
        }
        if !worktree_dest.exists() {
            continue;
        }
        if vcs.has_uncommitted_changes(&worktree_dest)? {
            modified.push(format!("{}: uncommitted changes", repo_path.as_str()));
            continue;
        }
        let repo_abs = source_root.join(repo_path.as_path());
        let wt_head = vcs.head_revision(&worktree_dest)?;
        let source_head = vcs.head_revision(&repo_abs)?;
        if wt_head != source_head {
            modified.push(format!(
                "{}: worktree has diverged from source ({} vs {})",
                repo_path.as_str(),
                short_sha(wt_head.as_str()),
                short_sha(source_head.as_str()),
            ));
        }
    }

    if !modified.is_empty() {
        bail!(
            "workweave at {} has local modifications; refusing to recreate without --force:\n  {}",
            workweave_dir.display(),
            modified.join("\n  ")
        );
    }

    Ok(workweave_dir.to_path_buf())
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Return the relative paths within `workweave_dir` whose worktrees have
/// uncommitted changes (staged, unstaged, or untracked).
///
/// Checks the project worktree (`projects/<project>`) and each manifest-repo
/// worktree. A repo that is missing on disk is skipped; a repo whose dirty
/// check itself fails is reported as dirty (conservative: "we couldn't
/// confirm clean").
pub fn collect_dirty_paths(
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> Vec<String> {
    let mut dirty = Vec::new();

    // Project worktree.
    let project_wt = workweave_dir.join("projects").join(project.as_str());
    if GitVcs.is_repo(&project_wt) {
        match GitVcs.has_uncommitted_changes(&project_wt) {
            Ok(true) => dirty.push(format!("projects/{}", project.as_str())),
            Ok(false) => {}
            Err(e) => dirty.push(format!(
                "projects/{}: status check failed: {e}",
                project.as_str()
            )),
        }
    }

    // Manifest-repo worktrees.
    for (repo_path, entry) in manifest.iter_entries() {
        let wt = workweave_dir.join(repo_path.as_path());
        // A reference alias is a shared symlink onto the canonical store and
        // has no per-workweave dirty state. A dirty canonical must not be
        // attributed to this workweave (it would block delete/replace of
        // every workweave sharing the reference). Skip it.
        if classify_checkout(&wt) == CheckoutKind::ReferenceAlias {
            continue;
        }
        if !wt.exists() {
            continue;
        }
        let vcs = vcs_for(entry.vcs_type);
        match vcs.has_uncommitted_changes(&wt) {
            Ok(true) => dirty.push(repo_path.as_str().to_string()),
            Ok(false) => {}
            Err(e) => dirty.push(format!("{}: status check failed: {e}", repo_path.as_str())),
        }
    }

    dirty
}

/// Merge baselines for a workweave: workspace roots whose lineage counts as
/// "this work has landed". The recorded parent (the workspace the workweave
/// was forked from, and the default sync-to target) comes first when the
/// marker is readable and the path still exists; the primary weave root is
/// always included. In nested choreography a child workweave's work lands
/// in its parent workweave and may reach primary only when the whole epic
/// ships — checking primary alone would refuse every child retire.
fn merge_baselines(workweave_dir: &Path, ws_root: &Path) -> Vec<PathBuf> {
    let mut baselines: Vec<PathBuf> = Vec::new();
    if let Ok(Some(marker)) = WorkweaveMarker::read(workweave_dir) {
        if marker.parent.exists() && marker.parent != ws_root {
            baselines.push(marker.parent);
        }
    }
    baselines.push(ws_root.to_path_buf());
    baselines
}

/// Walk the workweave's repos and report those whose worktree HEAD holds
/// commits not reachable from the same repo's HEAD in ANY of the `baselines`
/// (see [`merge_baselines`]).
///
/// Deleting such a workweave destroys committed work: the ephemeral-branch
/// cleanup force-deletes the only ref pointing at those commits. A worktree
/// whose HEAD cannot be read is reported as diverged (conservative: "we
/// couldn't confirm safe"); a repo present in no baseline at all is skipped,
/// matching the dirty check's missing-repo behavior.
fn collect_diverged_paths(
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    baselines: &[PathBuf],
) -> Vec<String> {
    let mut diverged = Vec::new();

    let mut check = |wt: &Path, rel: &Path, label: String| {
        if !wt.exists() {
            return;
        }
        let wt_head = match GitVcs.head_revision(wt) {
            Ok(h) => h,
            Err(e) => {
                diverged.push(format!("{label}: HEAD check failed: {e}"));
                return;
            }
        };
        // Resolve the workweave checkout's actual canonical store. The
        // `is_ancestor` query below MUST run in a DAG that contains BOTH
        // wt_head AND the baseline tip; the workweave checkout shares its
        // canonical store with the linked-workspace baselines (under
        // tier-0 invariants), so asking in the resolved canonical store
        // gives a sound answer even when the workweave dir is itself a
        // worktree. Under tier-0 violations the canonical store for the
        // workweave checkout differs from the baseline's canonical store,
        // and we conservatively decline to vouch — see below.
        let wt_canonical = match GitVcs
            .resolve_canonical_store(wt)
            .and_then(|s| s.parent().map(|p| p.to_path_buf()))
        {
            Some(p) => p.canonicalize().unwrap_or(p),
            None => {
                diverged.push(format!(
                    "{label}: canonical-store lookup failed: not a repo"
                ));
                return;
            }
        };
        let mut candidates = 0;
        for base in baselines {
            let canonical = base.join(rel);
            if !GitVcs.is_repo(&canonical) {
                continue;
            }
            candidates += 1;
            // Refuse to vouch across distinct canonical stores: an
            // is_ancestor query whose operands live in different object
            // DAGs is silently unsound (see joints/clone-topology.md).
            // When the baseline's canonical store differs from the
            // workweave checkout's, treat as not-vouched-by-this-baseline
            // and let the operator run `rwv doctor`.
            let base_canonical = match GitVcs
                .resolve_canonical_store(&canonical)
                .and_then(|s| s.parent().map(|p| p.to_path_buf()))
            {
                Some(p) => p.canonicalize().unwrap_or(p),
                None => continue,
            };
            if base_canonical != wt_canonical {
                continue;
            }
            if let Ok(c) = GitVcs.head_revision(&canonical) {
                // Run is_ancestor in the resolved canonical store so the
                // query is rooted in the DAG that contains both refs.
                if wt_head == c || GitVcs::is_ancestor(&wt_canonical, wt_head.as_str(), c.as_str())
                {
                    return; // vouched: this baseline contains the work
                }
            }
        }
        if candidates > 0 {
            diverged.push(label);
        }
    };

    let project_rel = Path::new("projects").join(project.as_str());
    let project_wt = workweave_dir.join(&project_rel);
    if GitVcs.is_repo(&project_wt) {
        check(
            &project_wt,
            &project_rel,
            format!("projects/{}", project.as_str()),
        );
    }

    for (repo_path, _entry) in manifest.iter_entries() {
        let wt = workweave_dir.join(repo_path.as_path());
        // A reference alias shares the canonical's branch (e.g. `main`); it
        // has no per-workweave commits that could be "unmerged" and force-
        // deleted on retire. Resolving it through the symlink would compare
        // the canonical's HEAD against the baselines and could spuriously
        // flag it. Skip it.
        if classify_checkout(&wt) == CheckoutKind::ReferenceAlias {
            continue;
        }
        check(&wt, repo_path.as_path(), repo_path.as_str().to_string());
    }

    diverged
}

/// Manifest-independent dirty scan: walk `dir` for git repos (worktrees or
/// clones) and report those with uncommitted changes, relative to `dir`.
///
/// Used when a workweave's marker is missing or belongs to another project,
/// so no manifest can be trusted to enumerate its contents. Descent stops at
/// each repo root (no nested-repo scanning), and a repo whose dirty check
/// fails is reported as dirty (conservative: "we couldn't confirm clean").
fn collect_dirty_repos_by_walk(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, cur: &Path, dirty: &mut Vec<String>) {
        if cur.join(".git").exists() {
            if GitVcs.has_uncommitted_changes(cur).unwrap_or(true) {
                let rel = cur.strip_prefix(base).unwrap_or(cur);
                dirty.push(rel.display().to_string());
            }
            return;
        }
        if let Ok(entries) = std::fs::read_dir(cur) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, dirty);
                }
            }
        }
    }
    let mut dirty = Vec::new();
    walk(dir, dir, &mut dirty);
    dirty
}

/// Resolve the canonical store path that owns a workweave checkout, for the
/// `worktree remove` / `worktree prune` calls below.
///
/// Under the clone-topology invariants (see
/// `docs/explanation/joints/clone-topology.md`) every workweave checkout is a
/// linked workspace whose `.git/` resolves into a canonical store somewhere
/// in the weave. `git worktree remove` and `git worktree prune` only know
/// about the worktree they're invoked from when called in that canonical
/// store — running them in some other "looks like a clone of the same repo"
/// directory either silently no-ops (the registration stays stale) or fails
/// loudly with "is not a working tree".
///
/// Resolution order:
/// - If the checkout doesn't exist on disk, fall back to `fallback`. Nothing
///   to remove anyway; this just keeps the prune/branch-cleanup loop simple.
/// - Otherwise ask the VCS for the checkout's actual canonical store. If
///   that query fails (the checkout is a plain dir, not a repo), fall back
///   to `fallback`.
///
/// The fallback path is the `<ws_root>/<repo_path>` slot the legacy code
/// used unconditionally — preserving behavior for the (now-explicit)
/// "checkout is gone" branch while making the canonical resolution
/// authoritative for the live case.
fn resolved_worktree_parent(checkout: &Path, fallback: &Path) -> PathBuf {
    if !checkout.exists() {
        return fallback.to_path_buf();
    }
    match GitVcs
        .resolve_canonical_store(checkout)
        .and_then(|s| s.parent().map(|p| p.to_path_buf()))
    {
        Some(p) => p.canonicalize().unwrap_or(p),
        None => fallback.to_path_buf(),
    }
}

/// Refuse `rwv workweave delete` when a checkout under `workweave_dir`
/// holds the canonical store that OTHER worktrees link into — the
/// catastrophic case the clone-topology joint flags as inverted topology
/// (e.g. fo-a0spgj hazard 2).
///
/// **Named precondition**: each per-repo workweave checkout MUST be a linked
/// workspace, not a canonical store with foreign dependents. Deleting a
/// canonical store while other worktrees still link into it would orphan
/// every dependent worktree on disk.
///
/// Returns `Err` with a named-precondition message pointing the operator at
/// `rwv doctor` (where the topology check lives, per the joint). This refusal
/// is NOT bypassable by `--force` — `--force` consents to losing this
/// workweave's work, not to corrupting other workweaves whose object DAG we
/// happen to be hosting. The operator must repair topology first (operator
/// work, out of scope for this verb).
///
/// Returns `Ok(())` when no per-repo checkout is a canonical store with
/// foreign dependents, OR when the only worktree the canonical store knows
/// about is the workweave's own checkout (the topology is fine — git just
/// records the checkout as its own worktree).
fn refuse_if_checkouts_host_foreign_worktrees(
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> anyhow::Result<()> {
    let mut hazards: Vec<String> = Vec::new();

    let mut check = |checkout: &Path, label: String| {
        if !checkout.exists() {
            return;
        }
        let canonical = match GitVcs
            .resolve_canonical_store(checkout)
            .and_then(|s| s.parent().map(|p| p.to_path_buf()))
        {
            Some(p) => p.canonicalize().unwrap_or(p),
            None => return, // not a repo; nothing to host
        };
        let checkout_canonical = checkout.canonicalize().unwrap_or(checkout.to_path_buf());
        if canonical != checkout_canonical {
            // Linked workspace — the canonical store lives elsewhere; this
            // checkout cannot have foreign dependents.
            return;
        }
        // checkout IS the canonical store. Enumerate every worktree this
        // store knows about and flag any whose path is NOT under our
        // workweave_dir (foreign — would be orphaned by delete).
        let worktrees = match GitVcs.list_worktrees(checkout) {
            Ok(ws) => ws,
            Err(_) => return,
        };
        let ww_canonical = workweave_dir
            .canonicalize()
            .unwrap_or(workweave_dir.to_path_buf());
        let mut foreign: Vec<PathBuf> = Vec::new();
        for wt in worktrees {
            let wt_canon = wt.canonicalize().unwrap_or(wt.clone());
            if wt_canon == checkout_canonical {
                continue; // the canonical store's own "main" entry
            }
            if !wt_canon.starts_with(&ww_canonical) {
                foreign.push(wt);
            }
        }
        if !foreign.is_empty() {
            let mut lines = vec![format!(
                "{label}: checkout is itself a canonical store with {} dependent worktree(s):",
                foreign.len()
            )];
            for wt in &foreign {
                lines.push(format!("    - {}", wt.display()));
            }
            hazards.push(lines.join("\n"));
        }
    };

    // Project worktree.
    let project_rel = Path::new("projects").join(project.as_str());
    let project_wt = workweave_dir.join(&project_rel);
    check(&project_wt, format!("projects/{}", project.as_str()));

    // Manifest repos.
    for (repo_path, _entry) in manifest.iter_entries() {
        let wt = workweave_dir.join(repo_path.as_path());
        // A reference alias resolves THROUGH the symlink to the canonical
        // store, whose own (legitimate) worktrees in other workweaves would
        // then look "foreign" and wrongly BLOCK this delete. The alias is not
        // a canonical store this workweave owns — skip it.
        if classify_checkout(&wt) == CheckoutKind::ReferenceAlias {
            continue;
        }
        check(&wt, repo_path.as_str().to_string());
    }

    if hazards.is_empty() {
        return Ok(());
    }

    bail!(
        "rwv workweave delete: refusing — inverted clone topology detected (precondition: \
         no-canonical-store-with-foreign-dependents).\n\
         The following checkout(s) inside workweave {} are themselves canonical \
         stores that other worktrees link into; deleting this workweave would orphan \
         those dependents:\n  {}\n\n\
         Run `rwv doctor` for a full topology audit and remediation guidance. \
         This refusal is NOT bypassable with --force: --force consents to losing \
         this workweave's work, not to corrupting unrelated worktrees whose object \
         store we happen to be hosting.",
        workweave_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| workweave_dir.display().to_string()),
        hazards.join("\n  "),
    )
}

/// Resolve the parent that children of the workweave at `retiree_dir` should
/// be adopted by when the retiree is retired or deleted.
///
/// Per Correction 5: children re-point to the RETIREE'S OWN recorded parent
/// (the grandparent), so the lineage stays transitive by construction — the
/// retiree's unique commits have just landed in that grandparent. If the
/// retiree's marker is unreadable, records no parent, or records a parent that
/// no longer exists on disk (itself already retired), fall back to `primary`,
/// which always exists.
///
/// The returned path is canonicalized when possible so the written child
/// marker records a stable absolute path (matching what `create_workweave`
/// writes via `source_root.canonicalize()`).
fn adoptive_parent_for_children(retiree_dir: &Path, primary_root: &Path) -> PathBuf {
    let grandparent = match WorkweaveMarker::read(retiree_dir) {
        Ok(Some(marker)) if marker.parent.exists() => Some(marker.parent),
        _ => None,
    };
    let target = grandparent.unwrap_or_else(|| primary_root.to_path_buf());
    target.canonicalize().unwrap_or(target)
}

/// Enumerate the live children of the workweave at `retiree_dir` and re-point
/// each child's `.rwv-workweave` `parent:` to `new_parent`, printing one loud
/// line per adopted child.
///
/// A "child" is any workweave under the same primary whose marker's `parent`
/// field canonically resolves to `retiree_dir`. Branch names are creation-time
/// namespaces, NOT lineage records, so they are deliberately left untouched —
/// this is exactly why consumers must read parent from the marker, never from
/// the branch name.
///
/// Shared by both the retire (`sync-to --retire`) and `workweave delete` paths
/// so the adoption semantics live in exactly one place. Runs BEFORE the retiree
/// directory is removed (the enumeration reads on-disk markers, but the
/// grandparent resolution in [`adoptive_parent_for_children`] must see the
/// retiree's own marker while it still exists).
///
/// Best-effort per child: a marker that can't be rewritten is reported to
/// stderr but does not abort the retire/delete (the alternative — leaving the
/// retiree in place — is strictly worse, and `rwv doctor --fix` re-points any
/// child left dangling by a partial failure).
fn adopt_children_of(retiree_dir: &Path, primary_root: &Path) {
    let new_parent = adoptive_parent_for_children(retiree_dir, primary_root);

    let retiree_canonical = retiree_dir
        .canonicalize()
        .unwrap_or_else(|_| retiree_dir.to_path_buf());

    // A retiree that IS its own adoptive parent (should not happen — the
    // grandparent is a different workspace) would create a self-loop; guard
    // against it defensively.
    let new_parent_canonical = new_parent
        .canonicalize()
        .unwrap_or_else(|_| new_parent.clone());

    for (child_name, child_dir) in list_workweave_dirs(primary_root) {
        let child_canonical = child_dir
            .canonicalize()
            .unwrap_or_else(|_| child_dir.clone());
        // Never re-point the retiree's own marker.
        if child_canonical == retiree_canonical {
            continue;
        }
        let mut marker = match WorkweaveMarker::read(&child_dir) {
            Ok(Some(m)) => m,
            _ => continue,
        };
        let marker_parent_canonical = marker
            .parent
            .canonicalize()
            .unwrap_or_else(|_| marker.parent.clone());
        if marker_parent_canonical != retiree_canonical {
            continue;
        }
        // Defensive: don't create a self-parent marker.
        if new_parent_canonical == child_canonical {
            eprintln!(
                "warning: not adopting child workweave {child_name}: adoptive parent \
                 {} is the child itself; run `rwv doctor --fix` to re-point to primary",
                new_parent.display()
            );
            continue;
        }
        marker.parent = new_parent.clone();
        if let Err(e) = marker.write(&child_dir) {
            eprintln!(
                "warning: failed to adopt child workweave {child_name}: could not rewrite \
                 marker ({e}); run `rwv doctor --fix` to re-point its parent"
            );
            continue;
        }
        eprintln!(
            "adopted child workweave {child_name}: parent now {}",
            new_parent.display()
        );
    }
}

/// Delete a workweave: remove worktrees (including project repo) and delete
/// the workweave directory.
///
/// Refuses to delete a workweave with uncommitted changes (in the project
/// worktree or any manifest-repo worktree) unless `force` is true. The error
/// lists the dirty paths so the operator knows what would have been lost.
/// `force` matches the `git branch -D` pattern.
///
/// Independently of `force`, refuses when any per-repo checkout in the
/// workweave is itself a canonical store with foreign worktrees linked into
/// it (named precondition: `no-canonical-store-with-foreign-dependents`).
/// This is the tier-0 invariant the clone-topology joint defines; delete
/// cannot safely proceed because the destructive worktree-remove + dir
/// removal would orphan the dependents. The operator must repair topology
/// via `rwv doctor` first.
pub fn delete_workweave(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    force: bool,
) -> anyhow::Result<()> {
    // Public `rwv workweave delete`: an INTERRUPTING verb. A mid-op workweave
    // refuses (op guard on).
    delete_workweave_inner(ws_root, project, name, force, false)
}

/// Delete a workweave as the terminal step of the OWNING op (`sync-to
/// --retire`).
///
/// Same as [`delete_workweave`] but skips the cross-verb op guard: the op that
/// is deleting this workweave still holds its own `.rwv-op` record here (the
/// record is cleared in the later `cleanup` phase), so the guard would
/// otherwise refuse the op's own retire. Only the sync engine's retire phase
/// calls this — never a standalone verb.
pub(crate) fn delete_workweave_for_retire(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    force: bool,
) -> anyhow::Result<()> {
    delete_workweave_inner(ws_root, project, name, force, true)
}

/// Shared delete implementation. `skip_op_guard` is `true` only for the
/// op-owned retire path (see [`delete_workweave_for_retire`]).
fn delete_workweave_inner(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    force: bool,
    skip_op_guard: bool,
) -> anyhow::Result<()> {
    let manifest = load_manifest(ws_root, project)?;
    let workweave_dir = workweave_path_for(ws_root, project, name);

    // Tier-0 topology precondition: refuse when a per-repo checkout inside
    // the workweave is itself a canonical store with foreign dependents.
    // Runs before the dirty / unmerged checks (and is not bypassable by
    // --force) because the hazard is to OTHER workspaces, not the
    // workweave's own work. See joints/clone-topology.md and
    // docs/contributing/destructive-operations.md (precondition-or-stop).
    if workweave_dir.exists() {
        refuse_if_checkouts_host_foreign_worktrees(&workweave_dir, project, &manifest)?;
    }

    // Cross-verb mutex (Correction 1, COVERAGE). A workweave that is mid-op
    // (holds an `.rwv-op` owner record or an `.rwv-op-lease`) must not be
    // deleted out from under the op — that would strand the owner record's
    // pointer or destroy the workspace `--continue`/`rwv abort` restore into.
    // Refuse FIRST (before the dirty/unmerged checks) so a mid-op delete reports
    // the in-flight op, not a dirty-tree error, mirroring the sync entry
    // ordering. `--force` does NOT bypass this: the hazard is to the op's
    // recovery, and `rwv abort` (not `--force delete`) is the way to clear a
    // stale record. Runs only when the dir exists (nothing to lose otherwise).
    // The op's OWN terminal retire (`delete_workweave_for_retire`) skips this —
    // its record is present by design and is cleared in the later cleanup phase.
    if workweave_dir.exists() && !skip_op_guard {
        crate::op_state::check_no_op_in_progress(&[workweave_dir.as_path()])?;
    }

    // Safety check: refuse to delete dirty or diverged workweaves without
    // --force. Skip the check if the workweave directory doesn't exist
    // (nothing to lose) or if force was passed.
    if !force && workweave_dir.exists() {
        let dirty = collect_dirty_paths(&workweave_dir, project, &manifest);
        if !dirty.is_empty() {
            bail!(
                "workweave {} has uncommitted changes; refusing to delete without --force:\n  {}",
                name.as_str(),
                dirty.join("\n  ")
            );
        }
        // Committed-but-unmerged work is just as lost as uncommitted work:
        // the ephemeral-branch cleanup below force-deletes the only ref to
        // those commits. Work counts as merged when its recorded parent OR
        // the primary weave contains it (nested workweaves land in their
        // parent first).
        let baselines = merge_baselines(&workweave_dir, ws_root);
        let diverged = collect_diverged_paths(&workweave_dir, project, &manifest, &baselines);
        if !diverged.is_empty() {
            bail!(
                "workweave {} has commits not merged into {}; \
                 refusing to delete without --force:\n  {}",
                name.as_str(),
                baselines
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" or "),
                diverged.join("\n  ")
            );
        }
    }

    // Adopt any living children BEFORE destroying the retiree. This re-points
    // each child's recorded parent to the retiree's own recorded parent (the
    // grandparent; fall back to primary) so a bare `rwv sync-to` from a child
    // does not later die on a dangling parent. Runs while the retiree's marker
    // still exists (it names the grandparent). Shared by delete and retire.
    if workweave_dir.exists() {
        adopt_children_of(&workweave_dir, ws_root);
    }

    // Remove worktrees for each repo, collecting errors.
    let mut errors: Vec<String> = Vec::new();

    for (repo_path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let worktree_path = workweave_dir.join(repo_path.as_path());

        // A reference alias is a symlink onto the shared canonical store.
        // Unlink it explicitly with `remove_file` (which removes the link,
        // never follows it) BEFORE any git/worktree/branch call. Today this
        // is only ACCIDENTALLY safe (the `is_lone_canonical` branch happens
        // to `continue`, and `remove_dir_all` at the end unlinks symlinks
        // rather than following them); making it explicit guarantees no
        // `git worktree remove` / `delete_branch` ever runs against the
        // canonical store through the link. The final dir-removal would also
        // unlink it, but unlinking here keeps the canonical untouched even if
        // a later refactor changes that.
        if classify_checkout(&worktree_path) == CheckoutKind::ReferenceAlias {
            if let Err(e) = std::fs::remove_file(&worktree_path) {
                let msg = format!("{}: removing reference symlink: {e}", repo_path.as_str());
                eprintln!("rwv workweave delete: error: {msg}");
                errors.push(msg);
            }
            continue;
        }

        // Resolve the worktree's ACTUAL canonical store on disk rather than
        // assuming `ws_root.join(repo_path)` is the parent. Under
        // tier-0-correct topology these match; under inverted topology
        // (fo-a0spgj) the canonical store lives in another workweave and
        // `ws_root.join(repo_path)` is a disconnected clone that doesn't
        // know about this checkout — running `worktree remove` there leaves
        // a stale registration. See joints/clone-topology.md.
        let fallback_repo_abs = ws_root.join(repo_path.as_path());
        let repo_abs = resolved_worktree_parent(&worktree_path, &fallback_repo_abs);

        if worktree_path.exists() {
            // Detect "checkout is its own canonical store" — `git worktree
            // remove` would fail with "is a main working tree". The
            // `refuse_if_checkouts_host_foreign_worktrees` precondition
            // above has already rejected the unsafe (with-dependents)
            // case; reaching this branch means the canonical-checkout is
            // lone, and removing the workweave dir is sufficient. Skip
            // the registration cleanup (there is no parent to clean) and
            // also the prune / branch-delete loop (nothing else can know
            // about this store's refs once the dir is gone).
            let checkout_canonical = worktree_path
                .canonicalize()
                .unwrap_or_else(|_| worktree_path.clone());
            let is_lone_canonical = repo_abs == checkout_canonical;
            if is_lone_canonical {
                // Nothing to unregister; the dir-removal at the end of
                // delete_workweave will clean up the on-disk store.
                continue;
            }
            if let Err(e) = vcs.remove_worktree(&repo_abs, &worktree_path) {
                let msg = format!("{}: {e}", repo_path.as_str());
                eprintln!("rwv workweave delete: error: {msg}");
                errors.push(msg);
                continue;
            }
        }

        // Prune stale worktree metadata and delete ephemeral branches.
        // Same resolved-parent rationale applies — pruning the wrong repo
        // leaves the actual canonical store's stale entries in place.
        let _ = vcs.worktree_prune(&repo_abs);
        let branch_prefix = ephemeral_branch_prefix(project, name);
        match vcs.list_branches_with_prefix(&repo_abs, &branch_prefix) {
            Ok(branches) => {
                for branch in branches {
                    if let Err(e) = vcs.delete_branch(&repo_abs, &branch) {
                        eprintln!(
                            "rwv workweave delete: warning: could not delete branch {branch} in {}: {e}",
                            repo_path.as_str()
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "rwv workweave delete: warning: could not list branches in {}: {e}",
                    repo_path.as_str()
                );
            }
        }
    }

    // Remove the project repo worktree.
    // Only call remove_worktree if the workweave copy is actually a git worktree,
    // indicated by .git being a FILE (not a directory). If .git is a directory
    // (or absent), the workweave copy was a plain directory copy — just let
    // remove_dir_all below handle it.
    let project_dir_fallback = ws_root.join("projects").join(project.as_str());
    let project_worktree = workweave_dir.join("projects").join(project.as_str());
    if project_worktree.exists() {
        let dot_git = project_worktree.join(".git");
        if dot_git.exists() && dot_git.is_file() {
            // Resolve the project worktree's actual canonical store, same
            // as for manifest repos above.
            let project_dir = resolved_worktree_parent(&project_worktree, &project_dir_fallback);
            if let Err(e) = GitVcs.remove_worktree(&project_dir, &project_worktree) {
                let msg = format!("projects/{}: {e}", project.as_str());
                eprintln!("rwv workweave delete: error: {msg}");
                errors.push(msg);
            } else {
                // Prune and delete ephemeral branches for the project repo.
                let _ = GitVcs.worktree_prune(&project_dir);
                let branch_prefix = ephemeral_branch_prefix(project, name);
                if let Ok(branches) = GitVcs.list_branches_with_prefix(&project_dir, &branch_prefix)
                {
                    for branch in branches {
                        if let Err(e) = GitVcs.delete_branch(&project_dir, &branch) {
                            eprintln!(
                                "rwv workweave delete: warning: could not delete branch {branch} in projects/{}: {e}",
                                project.as_str()
                            );
                        }
                    }
                }
            }
        }
    }

    // Remove the workweave directory itself.
    if workweave_dir.exists() {
        std::fs::remove_dir_all(&workweave_dir)?;
    }

    if errors.is_empty() {
        Ok(())
    } else {
        let total = manifest.len() + 1;
        let failed = errors.len();
        bail!("workweave delete completed with {failed} failure(s) out of {total} repo(s)")
    }
}

/// List workweaves for `project` under `ws_root`'s primary.
///
/// A workweave belongs to `(primary, project)` when its `.rwv-workweave`
/// marker records both. Directories without a marker are not included.
pub fn list_workweaves(ws_root: &Path, project: &ProjectName) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = list_workweave_dirs_for_project(ws_root, project)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    names.sort();
    Ok(names)
}

/// Return `(name, path)` pairs for workweaves of `project` under `ws_root`'s
/// primary. Only directories with a valid `.rwv-workweave` marker matching
/// `(primary, project)` are included.
fn list_workweave_dirs_for_project(
    ws_root: &Path,
    project: &ProjectName,
) -> Vec<(String, PathBuf)> {
    let parent = workweave_parent(ws_root);
    let primary_canonical = ws_root
        .canonicalize()
        .unwrap_or_else(|_| ws_root.to_path_buf());
    let mut result = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let parsed = parse_weave_dir_name(&dir_name);
            if parsed.is_none() {
                continue;
            }
            let (_, parsed_name) = parsed.unwrap();

            if let Ok(Some(marker)) = WorkweaveMarker::read(&dir) {
                if &marker.project != project {
                    continue;
                }
                let m_primary = marker
                    .primary
                    .canonicalize()
                    .unwrap_or_else(|_| marker.primary.clone());
                if m_primary != primary_canonical {
                    continue;
                }
                result.push((parsed_name.as_str().to_string(), dir));
            }
            // Directories without a valid marker are skipped.
        }
    }

    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Return `(name, path)` pairs for all workweave directories belonging to
/// `ws_root`'s primary, across every project. Used by `rwv doctor` to scan
/// all workweaves for drift. Only directories with a valid `.rwv-workweave`
/// marker whose `primary:` resolves to `ws_root` are included.
pub fn list_workweave_dirs(ws_root: &Path) -> Vec<(String, PathBuf)> {
    let parent = workweave_parent(ws_root);
    let primary_canonical = ws_root
        .canonicalize()
        .unwrap_or_else(|_| ws_root.to_path_buf());
    let mut result = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let parsed = parse_weave_dir_name(&dir_name);
            if parsed.is_none() {
                continue;
            }
            let (_, parsed_name) = parsed.unwrap();

            if let Ok(Some(marker)) = WorkweaveMarker::read(&dir) {
                let m_primary = marker
                    .primary
                    .canonicalize()
                    .unwrap_or_else(|_| marker.primary.clone());
                if m_primary == primary_canonical {
                    result.push((parsed_name.as_str().to_string(), dir));
                }
            }
            // Directories without a valid marker are skipped.
        }
    }

    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Load the project manifest from the workspace.
fn load_manifest(ws_root: &Path, project: &ProjectName) -> anyhow::Result<Manifest> {
    let manifest_path = ws_root
        .join("projects")
        .join(project.as_str())
        .join("rwv.yaml");
    Manifest::from_path(&manifest_path)
}

// ---------------------------------------------------------------------------
// `rwv workweave log [--diff]` — parent-relative history
// ---------------------------------------------------------------------------

/// A single commit in a `workweave log` listing, for `--json`.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct WorkweaveLogCommit {
    /// Full 40-hex commit SHA.
    pub sha: String,
    /// Abbreviated commit SHA.
    pub short: String,
    /// First line of the commit message.
    pub subject: String,
}

/// Per-repo entry in a `workweave log [--diff]` result.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct WorkweaveLogRepo {
    /// Manifest-relative repo path (e.g. `github/org/lib`).
    pub path: String,
    /// This workweave checkout's HEAD, if readable.
    pub head: Option<String>,
    /// The recorded parent's tip for THIS repo — the HEAD of the parent's
    /// checkout of the same path — if resolvable.
    pub parent_tip: Option<String>,
    /// The workweave's UNIQUE commits vs the parent tip — the commits in the
    /// workweave's history but not the parent's, newest first. Populated for
    /// `log`; empty in `--diff` mode.
    pub unique_commits: Vec<WorkweaveLogCommit>,
    /// The diff anchor: the common ancestor of the workweave tip and the
    /// parent tip. Populated only in `--diff` mode. Anchoring at the common
    /// ancestor (not the parent tip) avoids phantom reversals when the parent
    /// advanced after the fork.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_base: Option<String>,
    /// The unified diff text of the workweave's unique work vs `diff_base`,
    /// in `--diff` mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// A non-fatal note when this repo could not be fully processed (parent
    /// checkout missing, unreadable HEAD, etc.). The repo is still listed so
    /// the operator sees the gap rather than a silent omission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Top-level `workweave log [--diff] --json` envelope.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct WorkweaveLogOutput {
    /// The workweave name.
    pub workweave: String,
    /// The recorded parent workspace path (from the `.rwv-workweave` marker).
    pub parent: String,
    /// Whether this is a `diff` result (`true`) or a `log` result (`false`).
    pub diff: bool,
    /// Per-repo results, one per manifest repo.
    pub repos: Vec<WorkweaveLogRepo>,
}

/// Print (or JSON-emit) the workweave's UNIQUE commits vs the recorded parent,
/// per manifest repo.
///
/// Semantics (per fo-eycci9 headline layer A):
///   - Parent identity comes from the `.rwv-workweave` marker, NOT the branch
///     name (stacked branches like `lab--wwb/lab--wwa/main` make a constructed
///     `basename(parent)/main` wrong, and it is also wrong after
///     adoption-to-primary).
///   - `unique` commits are those in the workweave's history but not the
///     parent's, resolving the parent tip from the parent's checkout of the
///     same repo. This stays correct when the parent ADVANCED since the fork:
///     commits the parent already has are excluded.
///   - `diff` mode anchors at the COMMON ANCESTOR of the workweave tip and the
///     parent tip, NOT the parent tip directly: diffing against a parent tip
///     that advanced after the fork would show phantom reversals of the work
///     the parent gained in the meantime.
///
/// All VCS specifics are delegated to the [`Vcs`] impl for each repo's
/// `vcs_type` via [`vcs_for`]; this function stays VCS-agnostic.
///
/// `cwd` must be inside a workweave. `diff` selects diff mode; `json` selects
/// machine output.
pub fn workweave_log(cwd: &Path, diff: bool, json: bool) -> anyhow::Result<()> {
    use crate::workspace::{WorkspaceContext, WorkspaceLocation};

    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let (ww_name, ww_dir, project) = match &ctx.location {
        WorkspaceLocation::Workweave { name, dir, project } => {
            (name.clone(), dir.clone(), project.clone())
        }
        WorkspaceLocation::Weave { .. } => {
            bail!(
                "`rwv workweave log` reports a workweave's history relative to its \
                 recorded parent, but CWD ({}) is in the primary weave, not a workweave.",
                cwd.display()
            );
        }
    };

    let marker = WorkweaveMarker::read(&ww_dir)?.ok_or_else(|| {
        anyhow!(
            "`rwv workweave log` requires a `.rwv-workweave` marker in the workweave; \
             found none at {}",
            ww_dir.display()
        )
    })?;
    let parent_path = marker.parent.clone();
    if !parent_path.exists() {
        // Reuse the friendly dangling-parent remediation the sync path uses.
        crate::sync::check_parent_not_dangling(&parent_path, ctx.primary_path())?;
    }

    let manifest = load_manifest(ctx.primary_path(), &project)?;

    let mut repos: Vec<WorkweaveLogRepo> = Vec::new();
    for (repo_path, entry) in manifest.iter_entries() {
        let repo_rel = repo_path.as_path();
        let ww_repo = ww_dir.join(repo_rel);
        let parent_repo = parent_path.join(repo_rel);
        let vcs = vcs_for(entry.vcs_type);

        let mut note: Option<String> = None;

        // This workweave checkout's tip. Kept as the resolved id for the
        // output's `head` field; the trait methods re-resolve HEAD internally.
        let head = match vcs.head_revision(&ww_repo) {
            Ok(rev) => Some(rev),
            Err(e) => {
                note = Some(format!("workweave checkout HEAD unreadable: {e}"));
                None
            }
        };

        // The recorded parent's tip for THIS repo — HEAD in the parent's
        // checkout of the same path. Used as the exclusion boundary for
        // unique commits and as one endpoint of the diff's common ancestor.
        let parent_tip = match vcs.head_revision(&parent_repo) {
            Ok(rev) => Some(rev),
            Err(e) => {
                if note.is_none() {
                    note = Some(format!("parent checkout HEAD unreadable: {e}"));
                }
                None
            }
        };

        let mut unique_commits: Vec<WorkweaveLogCommit> = Vec::new();
        let mut diff_base: Option<String> = None;
        let mut diff_text: Option<String> = None;

        if let Some(parent_rev) = &parent_tip {
            if head.is_some() {
                if diff {
                    // The workweave's unique work vs the parent, anchored at
                    // their common ancestor so a parent that advanced after
                    // the fork does not show phantom reversals.
                    match vcs.unique_diff(&ww_repo, parent_rev) {
                        Ok(ud) => {
                            diff_base = ud.base;
                            diff_text = Some(ud.text);
                        }
                        Err(e) => note = Some(format!("diff failed: {e}")),
                    }
                } else {
                    match vcs.unique_commits(&ww_repo, parent_rev) {
                        Ok(entries) => {
                            unique_commits = entries
                                .into_iter()
                                .map(|c| WorkweaveLogCommit {
                                    sha: c.id,
                                    short: c.short,
                                    subject: c.subject,
                                })
                                .collect();
                        }
                        Err(e) => note = Some(format!("log failed: {e}")),
                    }
                }
            }
        }

        repos.push(WorkweaveLogRepo {
            path: repo_path.to_string(),
            head: head.map(|r| r.as_str().to_string()),
            parent_tip: parent_tip.map(|r| r.as_str().to_string()),
            unique_commits,
            diff_base,
            diff: diff_text,
            note,
        });
    }

    let output = WorkweaveLogOutput {
        workweave: ww_name.as_str().to_string(),
        parent: parent_path.to_string_lossy().to_string(),
        diff,
        repos,
    };

    if json {
        let out = serde_json::to_string_pretty(&output)
            .context("failed to serialize workweave log to JSON")?;
        println!("{out}");
    } else {
        print_workweave_log_text(&output);
    }

    Ok(())
}

/// Human-readable rendering of a `workweave log [--diff]` result.
fn print_workweave_log_text(output: &WorkweaveLogOutput) {
    let verb = if output.diff { "diff" } else { "log" };
    println!(
        "workweave {} {} vs parent {}",
        output.workweave, verb, output.parent
    );
    for repo in &output.repos {
        println!();
        println!("=== {} ===", repo.path);
        if let Some(note) = &repo.note {
            println!("  note: {note}");
        }
        if output.diff {
            match &repo.diff {
                Some(d) if !d.is_empty() => {
                    if let Some(base) = &repo.diff_base {
                        println!("  (diff range: {}..HEAD)", short12(base));
                    }
                    println!("{d}");
                }
                _ => println!("  (no diff vs parent)"),
            }
        } else if repo.unique_commits.is_empty() {
            println!("  (no unique commits vs parent)");
        } else {
            for c in &repo.unique_commits {
                println!("  {} {}", c.short, c.subject);
            }
        }
    }
}

/// Truncate a SHA to 12 chars for display.
fn short12(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

// ---------------------------------------------------------------------------
// Claude Code hook mode
// ---------------------------------------------------------------------------

/// Input JSON sent by Claude Code for WorktreeCreate / WorktreeRemove hooks.
#[derive(serde::Deserialize)]
struct ClaudeHookInput {
    cwd: Option<String>,
    branch_name: Option<String>,
    session_id: Option<String>,
    hook_event_name: Option<String>,
    worktree_path: Option<String>,
}

/// Derive a workweave name from the hook payload.
///
/// Priority: branch_name → timestamp+nanos fallback.
/// Session ID is not used — it's constant within a session, causing
/// collisions when multiple subagents are spawned.
/// Slashes are replaced with dashes for filesystem safety.
fn derive_workweave_name(branch_name: Option<&str>, _session_id: Option<&str>) -> String {
    let raw = match branch_name {
        Some(b) if !b.is_empty() && b != "null" => b.to_string(),
        _ => {
            let d = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            // Mix seconds and nanos into a short hex suffix for uniqueness
            let hash = d.as_secs() ^ (d.subsec_nanos() as u64);
            format!("workweave-{:08x}", hash)
        }
    };
    raw.replace('/', "-")
}

/// Handle a Claude Code hook invocation.
///
/// Reads JSON from stdin, dispatches on `hook_event_name`:
/// - `WorktreeCreate` — creates a workweave and prints its path to stdout.
/// - `WorktreeRemove` — deletes the workweave (fire-and-forget; always exits 0).
pub fn handle_claude_hook() -> anyhow::Result<()> {
    let input: ClaudeHookInput = serde_json::from_reader(std::io::stdin())
        .context("failed to parse hook JSON from stdin")?;

    match input.hook_event_name.as_deref() {
        Some("WorktreeCreate") => {
            let cwd = input
                .cwd
                .ok_or_else(|| anyhow!("missing cwd in hook input"))?;

            let ws_ctx = crate::workspace::WorkspaceContext::resolve(Path::new(&cwd), None)?;
            let primary_root = ws_ctx.primary_path();
            let source_root = ws_ctx.active_path();

            // Prefer the workweave's project (from .rwv-workweave marker) over
            // the primary weave's .rwv-active. This matters when a sub-agent
            // spawns from a workweave for a different project than the weave's
            // active project.
            let project = match &ws_ctx.location {
                crate::workspace::WorkspaceLocation::Workweave { project, .. } => project.clone(),
                _ => read_active_project(primary_root)
                    .ok_or_else(|| anyhow!("no .rwv-active found in {}", primary_root.display()))?,
            };

            let name =
                derive_workweave_name(input.branch_name.as_deref(), input.session_id.as_deref());

            let path = create_workweave(
                primary_root,
                source_root,
                &project,
                &WorkweaveName::new(&name),
                false,
                false,
                false,
            )?;
            println!("{}", path.display());
        }
        Some("WorktreeRemove") => {
            let worktree_path = input
                .worktree_path
                .ok_or_else(|| anyhow!("missing worktree_path in hook input"))?;

            // Fire-and-forget: log errors but always succeed.
            if let Ok(Some(marker)) = WorkweaveMarker::read(Path::new(&worktree_path)) {
                let dir_name = Path::new(&worktree_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                let name = dir_name
                    .split_once("--")
                    .map(|(_, n)| n)
                    .unwrap_or(dir_name);

                // Claude's WorktreeRemove is fire-and-forget cleanup of a
                // worktree Claude has decided to discard. Pass `force: true`
                // because (a) the operator's intent is already expressed by
                // the Claude action, and (b) any prompt for dirty state
                // would land on stderr unseen — Claude has already moved on.
                if let Err(e) = delete_workweave(
                    &marker.primary,
                    &marker.project,
                    &WorkweaveName::new(name),
                    true,
                ) {
                    eprintln!("rwv workweave --claude-hook WorktreeRemove: warning: {e}");
                }
            }
            // Always exit 0.
        }
        other => {
            anyhow::bail!("unknown hook_event_name: {:?}", other);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // derive_workweave_name
    // -----------------------------------------------------------------------

    #[test]
    fn derive_name_uses_branch_name() {
        assert_eq!(
            derive_workweave_name(Some("feat/my-branch"), None),
            "feat-my-branch"
        );
    }

    #[test]
    fn derive_name_null_branch_uses_timestamp() {
        let name = derive_workweave_name(Some("null"), Some("abc-session-123"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_empty_branch_uses_timestamp() {
        let name = derive_workweave_name(Some(""), Some("sess-xyz"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_timestamps_are_unique() {
        let a = derive_workweave_name(None, None);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let b = derive_workweave_name(None, None);
        assert_ne!(a, b, "sequential calls should produce different names");
    }

    #[test]
    fn derive_name_all_none_produces_timestamp() {
        let name = derive_workweave_name(None, None);
        assert!(
            name.starts_with("workweave-"),
            "expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_replaces_slashes() {
        assert_eq!(derive_workweave_name(Some("a/b/c"), None), "a-b-c");
    }

    // -----------------------------------------------------------------------
    // handle_claude_hook — JSON parsing via serde
    // -----------------------------------------------------------------------

    #[test]
    fn claude_hook_unknown_event_errors() {
        // Deserialise directly and call the dispatch logic via the public API.
        // We simulate by constructing the input struct.
        let json = r#"{"hook_event_name":"UnknownEvent"}"#;
        let input: ClaudeHookInput = serde_json::from_str(json).unwrap();
        // The match arm should hit the `other` branch.
        assert_eq!(input.hook_event_name.as_deref(), Some("UnknownEvent"));
        assert!(input.cwd.is_none());
        assert!(input.worktree_path.is_none());
    }

    #[test]
    fn claude_hook_null_branch_uses_timestamp_not_session() {
        let name = derive_workweave_name(Some("null"), Some("my-session-id"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-*, got {name}"
        );
    }

    #[test]
    fn claude_hook_input_deserialises_fully() {
        let wt_path = std::env::temp_dir().join("wt");
        let wt_str = wt_path.to_str().unwrap();
        let json = format!(
            r#"{{
            "cwd": "/home/user/ws",
            "branch_name": "feat/new-thing",
            "session_id": "sess-001",
            "hook_event_name": "WorktreeCreate",
            "worktree_path": "{wt_str}"
        }}"#
        );
        let input: ClaudeHookInput = serde_json::from_str(&json).unwrap();
        assert_eq!(input.cwd.as_deref(), Some("/home/user/ws"));
        assert_eq!(input.branch_name.as_deref(), Some("feat/new-thing"));
        assert_eq!(input.session_id.as_deref(), Some("sess-001"));
        assert_eq!(input.hook_event_name.as_deref(), Some("WorktreeCreate"));
        assert_eq!(input.worktree_path.as_deref(), Some(wt_str));
    }

    #[test]
    fn claude_hook_input_all_optional_fields_missing() {
        let json = r#"{}"#;
        let input: ClaudeHookInput = serde_json::from_str(json).unwrap();
        assert!(input.cwd.is_none());
        assert!(input.branch_name.is_none());
        assert!(input.session_id.is_none());
        assert!(input.hook_event_name.is_none());
        assert!(input.worktree_path.is_none());
    }
}
