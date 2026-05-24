//! `rwv push` — coordinated cross-repo push.
//!
//! The publisher-side counterpart to `rwv update`. Pushes manifest repos
//! first, then the project repo last — the project repo carries the
//! committed lock that pins manifest SHAs, so collaborators' `rwv fetch`
//! must never see a committed lock referencing unpushed manifest commits.
//! See fo-nxba7 for the full design.

use crate::git::GitVcs;
use crate::manifest::{Project, ProjectName, RepoEntry, RepoPath, Role};
use crate::vcs::{RawRevisionId, Vcs};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use anyhow::Context;
use std::path::{Path, PathBuf};

/// Run `rwv push` for the current workspace context.
///
/// Refuses if invoked from a workweave (workweave branches shouldn't leak
/// to shared remotes). Refuses if the project repo is off its canonical
/// branch. Walks the manifest pushing each non-fork repo on the
/// currently-checked-out branch via the role-conventional remote;
/// aggregates failures and only pushes the project repo if every manifest
/// push succeeded.
///
/// `force` propagates to every git push in the operation. `dry_run` prints
/// the plan without executing any pushes.
pub fn run_push(
    cwd: &Path,
    project_override: Option<ProjectName>,
    dry_run: bool,
    force: bool,
) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;

    // 1. Workspace precondition — refuse from a workweave.
    if let WorkspaceLocation::Workweave { name, .. } = &ctx.location {
        anyhow::bail!(
            "rwv push: refusing to push from workweave '{}'; \
             workweave branches shouldn't leak to shared remotes. \
             Run `rwv sync primary` to land changes on primary first.",
            name
        );
    }

    let project_name = ctx.require_active_project()?.clone();
    let primary_root = ctx.primary_path().to_path_buf();
    let project_dir = primary_root.join("projects").join(project_name.as_str());

    let project = Project::from_dir(&project_dir)
        .map_err(|e| anyhow::anyhow!("failed to load project '{}': {e}", project_name))?;

    let git = GitVcs;

    // 2. Project repo must be on its canonical branch. Publishing is a
    //    gateway-to-collaboration operation; require a stable primary
    //    context so collaborators see a predictable target branch.
    let project_canonical = git
        .default_branch(&project_dir)
        .with_context(|| format!("failed to determine canonical branch for {project_name}"))?;
    let project_current = git
        .current_ref(&project_dir)
        .with_context(|| format!("failed to read current branch for {project_name}"))?;
    let project_current = match project_current {
        Some(b) => b,
        None => anyhow::bail!(
            "rwv push: project repo at projects/{project_name}/ is on a detached HEAD; \
             check out the canonical branch ({project_canonical}) first"
        ),
    };
    if project_current.as_str() != project_canonical.as_str() {
        anyhow::bail!(
            "rwv push: project repo at projects/{project_name}/ is on branch '{project_current}', \
             not the canonical branch '{project_canonical}'. \
             Switch to '{project_canonical}' before pushing — publishing requires a stable primary context."
        );
    }

    // Snapshot manifest repos into a Vec so we can iterate twice (precondition
    // checks + push loop) without re-walking the BTreeMap.
    let manifest_repos: Vec<(RepoPath, RepoEntry)> = project
        .manifest
        .repositories
        .iter()
        .map(|(rp, e)| (rp.clone(), e.clone()))
        .collect();

    // 3. Lock-matches-state precondition. Compare each manifest repo's HEAD
    //    SHA to the lock's recorded SHA. Bail before touching the network so
    //    we don't half-publish state the committed lock doesn't reference.
    let lock_path = project_dir.join("rwv.lock");
    let lock_entries: std::collections::BTreeMap<RepoPath, RawRevisionId> =
        if let Some(raw_lock) = &project.lock {
            raw_lock
                .repositories
                .iter()
                .map(|(rp, entry)| (rp.clone(), entry.version.clone()))
                .collect()
        } else {
            std::collections::BTreeMap::new()
        };

    let mut lock_mismatches: Vec<String> = Vec::new();
    for (repo_path, _) in &manifest_repos {
        let repo_dir = primary_root.join(repo_path.as_path());
        if !repo_dir.exists() {
            lock_mismatches.push(format!(
                "{}: clone missing on disk; run `rwv fetch` first",
                repo_path.as_str()
            ));
            continue;
        }
        let head = match git.head_revision(&repo_dir) {
            Ok(h) => h,
            Err(e) => {
                lock_mismatches.push(format!(
                    "{}: failed to resolve HEAD: {e}",
                    repo_path.as_str()
                ));
                continue;
            }
        };
        let Some(lock_raw) = lock_entries.get(repo_path) else {
            lock_mismatches.push(format!(
                "{}: missing from {} — run `rwv lock` to record local state",
                repo_path.as_str(),
                lock_path.display()
            ));
            continue;
        };
        // Resolve the lock's raw version against the repo so tag-form lock
        // entries (e.g. `v1.2.3`) compare equal to a SHA-form HEAD.
        let lock_resolved = match git.resolve_revision(&repo_dir, lock_raw.as_str()) {
            Ok(r) => r,
            Err(e) => {
                lock_mismatches.push(format!(
                    "{}: lock entry '{}' could not be resolved in clone: {e}",
                    repo_path.as_str(),
                    lock_raw
                ));
                continue;
            }
        };
        if lock_resolved != head {
            lock_mismatches.push(format!(
                "{}: HEAD {} differs from lock {}",
                repo_path.as_str(),
                short_sha(head.as_str()),
                short_sha(lock_resolved.as_str()),
            ));
        }
    }

    if !lock_mismatches.is_empty() {
        eprintln!(
            "rwv push: {} repo(s) disagree with {}:",
            lock_mismatches.len(),
            lock_path.display()
        );
        for msg in &lock_mismatches {
            eprintln!("  - {msg}");
        }
        eprintln!(
            "Hint: run `rwv lock` to capture local state, \
             or `git checkout` in each repo to align with the lock."
        );
        anyhow::bail!("lock-state mismatch — refusing to push before clone state and lock agree");
    }

    // 4. Per-repo branch sanity. Detached HEAD is fatal; off-version branch
    //    is a warning (footgun catcher — operator may have a topic branch
    //    they intend to push; we warn but don't override).
    let mut branch_errors: Vec<String> = Vec::new();
    let mut plan: Vec<PushPlanItem> = Vec::with_capacity(manifest_repos.len());
    for (repo_path, entry) in &manifest_repos {
        let repo_dir = primary_root.join(repo_path.as_path());
        let current = match git.current_ref(&repo_dir) {
            Ok(c) => c,
            Err(e) => {
                branch_errors.push(format!(
                    "{}: failed to read current branch: {e}",
                    repo_path.as_str()
                ));
                continue;
            }
        };
        let branch = match current {
            Some(b) => b,
            None => {
                branch_errors.push(format!(
                    "{}: detached HEAD — checkout a branch before pushing",
                    repo_path.as_str()
                ));
                continue;
            }
        };
        if branch.as_str() != entry.version.as_str() {
            eprintln!(
                "rwv push: warning: {} is on branch '{}', manifest declares '{}'",
                repo_path.as_str(),
                branch,
                entry.version,
            );
        }
        plan.push(PushPlanItem {
            repo_path: repo_path.clone(),
            branch: branch.as_str().to_string(),
            role: entry.role,
        });
    }
    if !branch_errors.is_empty() {
        eprintln!(
            "rwv push: {} repo(s) cannot be pushed:",
            branch_errors.len()
        );
        for msg in &branch_errors {
            eprintln!("  - {msg}");
        }
        anyhow::bail!("aborted before network — fix per-repo branch state and retry");
    }

    // 5. Dry-run: print the plan in the format from fo-nxba7's bead body.
    if dry_run {
        println!("rwv push (dry-run):");
        for item in &plan {
            if item.role == Role::Fork {
                println!("  {}: would skip (Role::Fork)", item.repo_path.as_str());
            } else {
                println!(
                    "  {}: would push {} -> {}",
                    item.repo_path.as_str(),
                    item.branch,
                    remote_label(item.role),
                );
            }
        }
        println!(
            "  projects/{}: would push {} -> origin (last)",
            project_name, project_current,
        );
        return Ok(());
    }

    // 6. Manifest-repo push loop. Attempt all and collect — same shape as
    //    update.rs. Role::Fork is skipped here with an info line; the
    //    Vcs::push_with_role trait method stays neutral on Fork.
    let mut push_errors: Vec<String> = Vec::new();
    let mut pushed = 0usize;
    let mut skipped = 0usize;
    for item in &plan {
        if item.role == Role::Fork {
            println!(
                "rwv push: skipping {} (Role::Fork — push via PR)",
                item.repo_path.as_str()
            );
            skipped += 1;
            continue;
        }
        let repo_dir = primary_root.join(item.repo_path.as_path());
        println!(
            "rwv push: pushing {} ({} -> {})",
            item.repo_path.as_str(),
            item.branch,
            remote_label(item.role),
        );
        if let Err(e) = git.push_with_role(&repo_dir, item.role, force) {
            push_errors.push(format!("{}: git push failed: {e}", item.repo_path.as_str()));
        } else {
            pushed += 1;
        }
    }

    if !push_errors.is_empty() {
        eprintln!(
            "rwv push: {}/{} manifest repo(s) failed; project repo not pushed:",
            push_errors.len(),
            plan.len() - skipped,
        );
        for msg in &push_errors {
            eprintln!("  - {msg}");
        }
        anyhow::bail!(
            "manifest-repo push failures aborted before project-repo push; \
             manifest-side partial state may exist — inspect and retry"
        );
    }

    println!(
        "rwv push: pushed {} manifest repo(s); {} skipped (Role::Fork)",
        pushed, skipped
    );

    // 7. Project-repo push (gated). The project repo's committed lock pins
    //    the manifest SHAs we just pushed — pushing it last preserves the
    //    invariant that the remote-side lock never references unpushed
    //    objects. Use the same trait method so role policy stays in one
    //    place; the project repo is always Role::Primary at the trait
    //    layer (it's the canonical-tip carrier; not declared in any
    //    manifest).
    println!(
        "rwv push: pushing project repo projects/{} ({} -> origin)",
        project_name, project_current,
    );
    if let Err(e) = git.push_with_role(&project_dir, Role::Primary, force) {
        anyhow::bail!(
            "project-repo push failed after all manifest repos pushed cleanly: {e}. \
             Manifest-side state is published; the lock carrier is not. \
             Retry `rwv push` once the project-repo issue is resolved."
        );
    }

    println!("rwv push: done");
    Ok(())
}

/// One manifest repo's resolved plan entry — what branch it's on and what
/// role drives the remote selection. Built up-front so the dry-run output
/// and the actual push loop share the same shape.
struct PushPlanItem {
    repo_path: RepoPath,
    branch: String,
    role: Role,
}

/// Display the remote name for a role — matches the policy in
/// `git::remote_name_for_role` so dry-run output and actual pushes agree.
fn remote_label(role: Role) -> &'static str {
    match role {
        Role::Fork => "upstream",
        Role::Primary | Role::Dependency | Role::Reference => "origin",
    }
}

/// Abbreviate a SHA to 7 chars (matches lock-commit-message convention).
/// Pass-through for non-SHA strings (already abbreviated, tag-form, etc.).
fn short_sha(s: &str) -> String {
    if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        s[..7].to_string()
    } else {
        s.to_string()
    }
}

/// Resolve the project repo's path for tests that need the on-disk dir.
/// Kept `pub(crate)` so the integration tests in `tests/push_test.rs`
/// can mirror primary's layout when staging fixtures.
#[allow(dead_code)]
pub(crate) fn project_repo_dir(primary_root: &Path, project: &ProjectName) -> PathBuf {
    primary_root.join("projects").join(project.as_str())
}
