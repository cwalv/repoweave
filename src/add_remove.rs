//! `rwv add` and `rwv remove` — manage repos in a project manifest.

use crate::activate::{activate_intent, activate_workweave_intent};
use crate::integration_runner::missing_active_members;
use crate::manifest::{Manifest, ProjectName, RepoEntry, RepoPath, RepoUrl, Role, VcsType};
use crate::registry::{builtin_registries, Registry};
use crate::vcs::{vcs_for, EphemeralRefName, HeadAttachment, RefName, Vcs};
use crate::workspace::{project_dir, Checkout, WorkspaceContext};
use crate::workweave_index::RefRegistry;
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};

/// Resolve the project directory for an action verb.
///
/// The manifest (`rwv.toml`) is per-workspace state, so it lives under
/// [`WorkspaceContext::active_path`] — the workweave directory when CWD
/// is inside one, the primary weave when CWD is in the weave itself.
/// This mirrors the resolution `rwv lock` uses for `rwv.lock` (see
/// `src/lock.rs`): lock and manifest are siblings in the project repo,
/// so they follow the same per-workspace ownership rule. The
/// `lock-as-derived` joint at
/// `docs/explanation/joints/lock-as-derived.md` is the conceptual
/// reference.
///
/// The clone destination, in contrast, stays at [`WorkspaceContext::primary_path`]
/// — clones are global infrastructure shared across workweaves via
/// `git worktree` (canonical store at primary, linked workspaces in
/// workweaves; see `docs/explanation/joints/clone-topology.md`).
/// Callers compose `find_project` (workspace-owned state) with
/// `primary_path()` (global clones) explicitly.
///
/// The name travels with the directory because a [`ProjectName`] may carry a
/// separator (`chatly/web-app`); recovering it from the directory's last
/// component would silently collapse that to `web-app` and load a different
/// project's manifest.
fn find_project(ctx: &WorkspaceContext) -> anyhow::Result<(ProjectName, PathBuf)> {
    let name = ctx.require_active_project_on_disk()?.clone();
    let dir = project_dir(ctx.active_path(), name.as_str());
    Ok((name, dir))
}

/// Create a worktree of `repo_path` in the workweave directory, pointing at
/// the canonical clone at `primary_root`.
///
/// Used by `rwv add` from a workweave so the workweave gets the new repo's
/// worktree as part of the add (mirrors the pattern in
/// `sync::materialize_missing_repo`). Creating an attachment where there was
/// none is a **birth**: it attaches at the revision the verb is
/// materializing, and is never followed by a move to reach the intended
/// revision. The name comes from [`EphemeralRefName::mint`], the same and
/// only minter `workweave create` uses — deriving one here instead is what
/// let three copies of the derivation disagree.
///
/// It also **emits an ownership receipt**, which is what makes a later
/// `workweave delete` visit this ref at all: under R2 delete destroys the
/// recorded set, and a ref added this way used to be reachable only by the
/// prefix glob that no longer authorizes anything.
///
/// If the canonical clone has no HEAD (e.g. `rwv add --new` produced an
/// empty `git init`), the worktree creation is skipped — `git worktree add`
/// against an unborn HEAD would fail, and the user can materialize the
/// worktree after the first commit via `rwv sync`.
fn create_worktree_in_workweave(
    vcs: &dyn Vcs,
    primary_root: &Path,
    canonical_clone: &Path,
    workweave_dir: &Path,
    repo_path: &RepoPath,
    project: &ProjectName,
    workweave_name: &crate::manifest::WorkweaveName,
) -> anyhow::Result<()> {
    let dest = workweave_dir.join(repo_path.as_path());
    if dest.exists() {
        // Nothing to do — operator may have pre-populated the workweave,
        // or this is a re-run of `rwv add` for an already-materialized repo.
        return Ok(());
    }

    // Skip if the canonical has no HEAD (empty `git init` from --new).
    let start = match vcs.head_revision(canonical_clone) {
        Ok(start) => start,
        Err(_) => {
            eprintln!(
                "rwv add: canonical clone at {} has no commits yet; \
                 skipping worktree creation in workweave \
                 (commit upstream and run `rwv sync` to materialize)",
                canonical_clone.display()
            );
            return Ok(());
        }
    };

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let mut registry = RefRegistry::for_project(primary_root, project);
    let ephemeral = EphemeralRefName::mint(project, workweave_name);
    let outcome = crate::workweave::birth_ephemeral_worktree(
        vcs,
        &mut registry,
        canonical_clone,
        &dest,
        &ephemeral,
        start,
    )
    .with_context(|| {
        format!(
            "failed to create worktree at {} from canonical clone {}",
            dest.display(),
            canonical_clone.display()
        )
    })?;

    if let Some(e) = outcome.failure {
        // The receipt stands and names the ref, so a later delete or doctor
        // pass can reconcile whatever git left behind. Nothing is deleted
        // here: a DESTROY needs a warrant this path does not hold.
        return Err(e).with_context(|| {
            format!(
                "failed to create worktree at {} from canonical clone {}",
                dest.display(),
                canonical_clone.display()
            )
        });
    }

    Ok(())
}

/// Run the appropriate activation pass for the current checkout kind.
///
/// `rwv add`/`rwv remove` are **intent verbs**: they mutate `rwv.toml`, then
/// regenerate the integrations' managed/generated files so the new content can
/// be committed alongside the manifest change.
/// In a workweave we still regenerate (the workweave is a view onto the
/// project repo — symlinks write through to it) but skip install hooks; in
/// primary we run the full intent-mode activation.
///
/// Regeneration is withheld when an active repo the manifest declares is not
/// on disk: the managed files are authored from the repos the run can see, so
/// over a partial member set they would be rewritten without the rest. The
/// manifest change still lands, and `rwv doctor --fix` regenerates once the
/// member set is whole.
fn activate_for_workspace(ctx: &WorkspaceContext, project: &ProjectName) -> anyhow::Result<()> {
    let root = ctx.active_path();
    let manifest_path = project_dir(root, project.as_str()).join(Manifest::FILE_NAME);
    let manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    let missing = missing_active_members(root, manifest.iter_entries());
    if !missing.is_empty() {
        eprintln!(
            "warning: {} manifest repo(s) not on disk; managed files left unchanged:",
            missing.len()
        );
        for path in &missing {
            eprintln!("  - {path}");
        }
        eprintln!("run `rwv fetch` to materialize them, then `rwv doctor --fix` to regenerate.");
        return Ok(());
    }

    match &ctx.checkout {
        Checkout::Workweave { dir, .. } => activate_workweave_intent(project.as_str(), dir),
        Checkout::Primary { .. } => activate_intent(project.as_str(), ctx),
    }
}

/// Execute `rwv add URL [--role=ROLE]`.
///
/// `ctx` is the already-resolved invocation context (with `--project`
/// baked in when passed). Handlers must not re-resolve.
pub fn run_add(url: &str, role: Role, ctx: &WorkspaceContext) -> anyhow::Result<()> {
    // `rwv add` mints the manifest entry, so the backend is an input to the
    // verb rather than a lookup: one value feeds both the handle this verb
    // operates through and the `vcs_type` it records.
    let vcs_type = VcsType::Git;
    let vcs = vcs_for(vcs_type);
    let (project, project_dir) = find_project(ctx)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let parsed_url: RepoUrl = url.parse()?;

    // A non-URL argument may name a clone already on disk. Local-path
    // resolution scans primary for it — that is the canonical layout
    // regardless of CWD.
    if !parsed_url.is_url() {
        let candidate = ctx.primary_path().join(url);
        if candidate.is_dir() {
            // Warn when this clone path is already registered by another project
            // before adding it here (local-path arm). Same shared-clone detection
            // as the URL arm.
            {
                let repo_path = RepoPath::new(url)?;
                warn_if_shared_clone(ctx.primary_path(), &project, &repo_path, &candidate, role);
            }
            run_add_from_local_path(
                vcs.as_ref(),
                vcs_type,
                url,
                &candidate,
                role,
                &manifest_path,
            )?;
            // Local-path add doesn't clone, but if we are in a workweave the
            // operator may still expect the workweave to see the repo as a
            // worktree. Mirror the URL path's worktree-creation step.
            if let Checkout::Workweave {
                name, dir, project, ..
            } = &ctx.checkout
            {
                let repo_path = RepoPath::new(url)?;
                let canonical = ctx.primary_path().join(repo_path.as_path());
                if canonical.exists() {
                    create_worktree_in_workweave(
                        vcs.as_ref(),
                        ctx.primary_path(),
                        &canonical,
                        dir,
                        &repo_path,
                        project,
                        name.require(dir, project)?,
                    )?;
                }
            }
            return activate_for_workspace(ctx, &project);
        }
    }

    let local_path = parsed_url
        .local_path()
        .or_else(|| derive_local_path_from_url(url))
        .ok_or_else(|| {
            let registries = builtin_registries();
            let names = registries
                .iter()
                .map(|r| r.name().as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!(
                "Error: unrecognized URL '{url}' — could not derive a local path \
                 (supported registries: {names})"
            )
        })?;

    let repo_path = RepoPath::new(local_path)?;

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if manifest.contains_repo(&repo_path) {
        eprintln!(
            "Repository already exists in manifest at '{}'",
            repo_path.as_str()
        );
        return Ok(());
    }

    // Clone the repo if it doesn't exist on disk. The clone always lives
    // at primary's canonical path — the canonical store, in tier-0
    // topology terms; workweaves link into it via `git worktree` rather
    // than holding their own clones (see
    // `docs/explanation/joints/clone-topology.md`).
    let dest = ctx.primary_path().join(repo_path.as_path());

    // Warn when this clone path is already registered by another project.
    // The physical clone is shared; the operator should know. This is not a
    // refusal — sharing is legitimate and supported.
    {
        warn_if_shared_clone(ctx.primary_path(), &project, &repo_path, &dest, role);
    }
    if dest.exists() {
        eprintln!(
            "Directory already exists at '{}', skipping clone",
            dest.display()
        );
    } else {
        // Create parent directories.
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        vcs.clone_repo(url, &dest)
            .with_context(|| format!("failed to clone '{}' into {}", url, dest.display()))?;
    }

    let Some(remote_default) = vcs
        .remote_default_branch(&dest)
        .with_context(|| format!("failed to determine default branch for {}", dest.display()))?
    else {
        anyhow::bail!(
            "rwv add: '{}' at {}: {}, then re-run `rwv add {}`",
            repo_path.as_str(),
            dest.display(),
            vcs.remote_default_branch_repair_hint(),
            url
        );
    };
    let default_branch = RefName::new(remote_default.local_counterpart().to_string());

    // Add entry to manifest.
    let entry = RepoEntry {
        vcs_type,
        url: parsed_url,
        version: default_branch,
        role,
    };
    manifest.insert_repo(repo_path.clone(), entry);

    // Write back the manifest (per-workspace state: workweave's own copy when
    // CWD is in one, primary's otherwise).
    manifest.write(&manifest_path)?;

    eprintln!("Added '{}' to manifest", repo_path.as_str());

    // In a workweave, also create a worktree at the workweave so the new
    // repo is materialized there.
    if let Checkout::Workweave {
        name, dir, project, ..
    } = &ctx.checkout
    {
        create_worktree_in_workweave(
            vcs.as_ref(),
            ctx.primary_path(),
            &dest,
            dir,
            &repo_path,
            project,
            name.require(dir, project)?,
        )?;
    }

    // Re-run activation so ecosystem files (Cargo.toml, package.json, etc.) are updated.
    activate_for_workspace(ctx, &project)?;

    Ok(())
}

/// Handle `rwv add <local-path>` where the argument is a relative path to an
/// existing directory under the workspace root.  Infers the URL by reading the
/// clone's `origin` remote.
fn run_add_from_local_path(
    vcs: &dyn Vcs,
    vcs_type: VcsType,
    path_arg: &str,
    clone_dir: &Path,
    role: Role,
    manifest_path: &Path,
) -> anyhow::Result<()> {
    // Read the recorded URL from the existing clone.
    let raw_url = vcs
        .remote_url(clone_dir)
        .with_context(|| {
            format!(
                "could not determine the {} URL for '{}'",
                vcs.conventional_remote_name(),
                clone_dir.display()
            )
        })?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "'{}' has no {} remote to read a URL from",
                clone_dir.display(),
                vcs.conventional_remote_name()
            )
        })?;

    // Normalise bare absolute paths to file:// URLs so manifests are
    // consistent regardless of how the clone was created.
    let origin_url = if raw_url.starts_with('/') {
        format!("file://{raw_url}")
    } else if !raw_url.contains("://") && Path::new(&raw_url).is_absolute() {
        // The platform's other absolute spelling: a drive-letter path git
        // reports verbatim. A file URL takes forward slashes and roots the
        // drive letter behind a third slash.
        format!("file:///{}", raw_url.replace('\\', "/"))
    } else {
        raw_url
    };

    let repo_path = RepoPath::new(path_arg)?;

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if manifest.contains_repo(&repo_path) {
        eprintln!(
            "Repository already exists in manifest at '{}'",
            repo_path.as_str()
        );
        return Ok(());
    }

    let Some(remote_default) = vcs.remote_default_branch(clone_dir).with_context(|| {
        format!(
            "failed to determine default branch for {}",
            clone_dir.display()
        )
    })?
    else {
        anyhow::bail!(
            "rwv add: '{}' at {}: {}, then re-run `rwv add {}`",
            repo_path.as_str(),
            clone_dir.display(),
            vcs.remote_default_branch_repair_hint(),
            path_arg
        );
    };
    let default_branch = RefName::new(remote_default.local_counterpart().to_string());

    // Add entry to manifest using the inferred origin URL.
    let entry = RepoEntry {
        vcs_type,
        url: origin_url.parse()?,
        version: default_branch,
        role,
    };
    manifest.insert_repo(repo_path.clone(), entry);

    manifest.write(manifest_path)?;

    eprintln!(
        "Added '{}' to manifest (url: {})",
        repo_path.as_str(),
        origin_url
    );
    Ok(())
}

/// Execute `rwv remove PATH [--delete] [--delete-shared-clone]`.
///
/// `ctx` is the already-resolved invocation context. Handlers must not
/// re-resolve.
pub fn run_remove(
    path: &str,
    delete: bool,
    delete_shared_clone: bool,
    ctx: &WorkspaceContext,
) -> anyhow::Result<()> {
    let (project, project_dir) = find_project(ctx)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);

    let repo_path = RepoPath::new(path)?;

    // Load existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    let Some(removed) = manifest.remove_repo(&repo_path) else {
        bail!("Error: path '{}' not found in manifest", repo_path.as_str());
    };

    // Before writing anything, check for cross-project references when --delete
    // is requested.  If another project references the repo and
    // --delete-shared-clone is not set, bail early so the manifest is left
    // untouched. The cross-project
    // scan walks primary's `projects/` directory — that is the canonical
    // enumeration of project manifests regardless of CWD.
    if delete {
        let repo_dir = ctx.primary_path().join(repo_path.as_path());
        if repo_dir.exists() {
            // Pass the primary-side project_dir so the scan correctly skips
            // the active project even when CWD is in a workweave (where
            // `project_dir` lives under the workweave, not primary).
            let referencing_projects =
                find_other_projects_referencing(ctx.primary_path(), &project, &repo_path);

            if !referencing_projects.is_empty() {
                for proj in &referencing_projects {
                    eprintln!("warning: repo also referenced by project '{proj}'");
                }
                if !delete_shared_clone {
                    anyhow::bail!(
                        "refusing to delete '{}': referenced by other projects. Remove the entry from those projects first, or use `--delete-shared-clone` if you intend to delete the shared clone anyway.",
                        repo_path.as_str()
                    );
                }
            }

            // R4, checked in the same pre-flight and for the same reason:
            // a refused DESTROY-STORE must leave the manifest as it found it,
            // so `rwv remove --delete` is retryable after the operator clears
            // the claims rather than leaving them holding a store the manifest
            // no longer declares.
            refuse_claimed_store(
                vcs_for(removed.vcs_type).as_ref(),
                ctx.primary_path(),
                &repo_dir,
            )?;
        }
    }

    // Write back the manifest (after all pre-flight checks pass).
    manifest.write(&manifest_path)?;

    eprintln!("Removed '{}' from manifest", repo_path.as_str());

    // Re-run activation so ecosystem files (Cargo.toml, package.json, etc.) are updated.
    activate_for_workspace(ctx, &project)?;

    // Optionally delete the clone directory. Always targets primary's
    // canonical path — `--delete` semantics mean "remove the shared clone",
    // independent of CWD's workspace. Operators in a workweave who want to
    // drop just their workweave's view should use `rwv remove` without
    // `--delete` (manifest-only) plus `rwv workweave delete` to clean up
    // the worktree.
    if delete {
        let repo_dir = ctx.primary_path().join(repo_path.as_path());
        if repo_dir.exists() {
            std::fs::remove_dir_all(&repo_dir)
                .with_context(|| format!("failed to delete directory {}", repo_dir.display()))?;
            eprintln!("Deleted '{}'", repo_dir.display());
        }
    }

    Ok(())
}

/// R4: refuse a DESTROY-STORE while the store is still claimed.
///
/// `remove --delete` deletes an entire ref store and its object database,
/// which destroys every ref and every object at once — so no ref-level rule
/// can gate it, and none is allowed to be read as permitting it. R4 names the
/// two claims that must be gone first:
///
/// - **no live worktree registered against the store.** Every workweave
///   checkout of this repo is a linked worktree of this store, so deleting it
///   guts live workweaves — their `.git` files point into the directory being
///   removed. `git worktree list` reports the store itself plus one line per
///   linked worktree; anything beyond the first is a live claim.
/// - **every receipt keyed to the store retracted.** A standing receipt says
///   rwv created a ref here and has not destroyed it, which is exactly the
///   per-ref DESTROY discipline not having run dry yet. Receipts are checked
///   across *all* projects in the weave, not just the active one: a clone is
///   shared by path, so another project's workweave can hold a ref in this
///   same store.
///
/// The verb's own named preconditions (dirty state, unpushed work) sit on top
/// of this and are separate work, still open.
fn refuse_claimed_store(vcs: &dyn Vcs, primary_root: &Path, repo_dir: &Path) -> anyhow::Result<()> {
    let mut claims: Vec<String> = Vec::new();

    match vcs.list_worktrees(repo_dir) {
        Ok(worktrees) => {
            let store = repo_dir
                .canonicalize()
                .unwrap_or_else(|_| repo_dir.to_path_buf());
            for wt in worktrees {
                let wt_canonical = wt.canonicalize().unwrap_or_else(|_| wt.clone());
                if wt_canonical != store {
                    claims.push(format!("live worktree registered at {}", wt.display()));
                }
            }
        }
        // Not a repo, or git could not answer. A directory that is not a ref
        // store is not a DESTROY-STORE, and a store that cannot be
        // interrogated is not one this verb may assume is unclaimed.
        Err(e) => {
            if vcs.is_repo(repo_dir) {
                anyhow::bail!(
                    "refusing to delete '{}': it is a repo whose worktree registrations \
                     could not be read ({e}), so rwv cannot establish that no live \
                     worktree is using it.",
                    repo_dir.display()
                );
            }
        }
    }

    for project in crate::workspace::discover_projects(primary_root) {
        let registry = RefRegistry::for_project(primary_root, &project);
        match registry.list_for_store(repo_dir) {
            Ok(receipts) => claims.extend(receipts.into_iter().map(|r| {
                format!(
                    "ownership receipt for branch {r} (project {})",
                    project.as_str()
                )
            })),
            Err(e) => anyhow::bail!(
                "refusing to delete '{}': the ownership receipts for project `{}` could \
                 not be read ({e}), so rwv cannot establish that no ref it created still \
                 lives in this store.",
                repo_dir.display(),
                project.as_str()
            ),
        }
    }

    if claims.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to delete '{}': the store is still claimed:\n  {}\n\n\
         Deleting it would take every ref and object with it at once. Delete the \
         workweaves that hold these first (`rwv workweave <project> delete <name>`), \
         which removes their worktrees and retracts their receipts, then re-run.",
        repo_dir.display(),
        claims.join("\n  "),
    )
}

/// Execute `rwv add PATH --new`.
///
/// Instead of cloning from a URL, this creates a new repo at the canonical
/// path by running `git init`. The URL is inferred from the path convention
/// via registries (e.g., `github/owner/repo` → `https://github.com/owner/repo.git`).
/// The repo is added to the manifest with role `primary`.
///
/// `ctx` is the already-resolved invocation context. Handlers must not
/// re-resolve.
pub fn run_add_new(path_arg: &str, ctx: &WorkspaceContext) -> anyhow::Result<()> {
    // `rwv add` mints the manifest entry, so the backend is an input to the
    // verb rather than a lookup: one value feeds both the handle this verb
    // operates through and the `vcs_type` it records.
    let vcs_type = VcsType::Git;
    let vcs = vcs_for(vcs_type);
    let (project, project_dir) = find_project(ctx)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);

    // Validate that the argument looks like a path (registry/owner/repo).
    let segments: Vec<&str> = path_arg.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        bail!(
            "Error: '{}' does not look like a valid repo path (expected registry/owner/repo)",
            path_arg
        );
    }

    // Try to infer the URL from the path via registries.
    let owned_registries = builtin_registries();
    let registry_refs: Vec<&dyn Registry> = owned_registries.iter().map(|r| r.as_ref()).collect();

    let url = infer_url_from_path(path_arg, &registry_refs).ok_or_else(|| {
        anyhow::anyhow!(
            "Error: could not infer a URL from path '{}' — no matching registry",
            path_arg
        )
    })?;

    let repo_path = RepoPath::new(path_arg)?;

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if manifest.contains_repo(&repo_path) {
        eprintln!(
            "Repository already exists in manifest at '{}'",
            repo_path.as_str()
        );
        return Ok(());
    }

    // Create the directory and run git init at primary's canonical path
    // (clones are global infrastructure).
    let dest = ctx.primary_path().join(repo_path.as_path());

    // Warn when this repo path is already registered by another project with
    // a potentially different role. The physical init'd repo is shared; the
    // operator should know. This is not a refusal.
    {
        warn_if_shared_clone(ctx.primary_path(), &project, &repo_path, &dest, Role::Owned);
    }
    if dest.exists() {
        eprintln!(
            "Directory already exists at '{}', skipping init",
            dest.display()
        );
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        vcs.init_repo(&dest)
            .with_context(|| format!("failed to init repo at {}", dest.display()))?;
    }

    let default_branch = match vcs.head_attachment(&dest)? {
        HeadAttachment::Unborn(u) => RefName::new(u.to_string()),
        HeadAttachment::Attached(a) => RefName::new(a.to_string()),
        HeadAttachment::Detached(_) => anyhow::bail!(
            "rwv add: repo at {} has a detached HEAD right after `git init`; \
             inspect with `git -C {} status` before retrying `rwv add {} --new`",
            dest.display(),
            dest.display(),
            path_arg
        ),
    };

    // Add entry to manifest with role primary.
    let entry = RepoEntry {
        vcs_type,
        url,
        version: default_branch,
        role: Role::Owned,
    };
    manifest.insert_repo(repo_path.clone(), entry);

    // Write back the manifest (per-workspace state).
    manifest.write(&manifest_path)?;

    eprintln!("Added new repo '{}' to manifest", repo_path.as_str());

    // In a workweave, attempt to create a worktree at the workweave so the
    // new repo is materialized there. `git init` produces an unborn HEAD,
    // so create_worktree_in_workweave silently skips until the first commit
    // lands upstream (operator can then `rwv sync`).
    if let Checkout::Workweave {
        name, dir, project, ..
    } = &ctx.checkout
    {
        create_worktree_in_workweave(
            vcs.as_ref(),
            ctx.primary_path(),
            &dest,
            dir,
            &repo_path,
            project,
            name.require(dir, project)?,
        )?;
    }

    // Re-run activation so ecosystem files (Cargo.toml, package.json, etc.) are updated.
    activate_for_workspace(ctx, &project)?;

    Ok(())
}

/// Scan `projects/*/rwv.toml` (excluding `active_project_dir`) and return the
/// names of any projects that reference `repo_path`.
fn find_other_projects_referencing(
    workspace_root: &Path,
    active_project: &ProjectName,
    repo_path: &RepoPath,
) -> Vec<ProjectName> {
    find_other_projects_with_roles(workspace_root, active_project, repo_path)
        .into_iter()
        .map(|(name, _role)| name)
        .collect()
}

/// Walk every project in the weave except `active_project` and return the
/// `(project, role)` pairs for those that register `repo_path`.
///
/// Used by `rwv add` to emit shared-clone warnings and by `rwv remove
/// --delete` to refuse a clone another project still declares.
fn find_other_projects_with_roles(
    workspace_root: &Path,
    active_project: &ProjectName,
    repo_path: &RepoPath,
) -> Vec<(ProjectName, Role)> {
    let mut referencing: Vec<(ProjectName, Role)> = Vec::new();

    for project in crate::workspace::discover_projects(workspace_root) {
        if &project == active_project {
            continue;
        }
        let manifest_path = project_dir(workspace_root, project.as_str()).join(Manifest::FILE_NAME);
        if let Ok(manifest) = Manifest::from_path(&manifest_path) {
            if let Some(entry) = manifest.get_entry(repo_path) {
                referencing.push((project, entry.role));
            }
        }
    }

    referencing
}

/// Emit `[warning] add: shared-clone` messages when `repo_path` is already
/// registered by other projects in this weave.
///
/// `this_role` is the role the caller is adding `repo_path` as; it is
/// included in the warning so the operator understands the full picture.
///
/// `workspace_root` is primary — even from a workweave the canonical
/// manifest set lives under primary's `projects/`.
fn warn_if_shared_clone(
    workspace_root: &Path,
    active_project: &ProjectName,
    repo_path: &RepoPath,
    clone_dest: &Path,
    this_role: Role,
) {
    let siblings = find_other_projects_with_roles(workspace_root, active_project, repo_path);
    for (other_project, other_role) in &siblings {
        eprintln!(
            "[warning] add: clone {} is already registered by project '{}' with role {}; \
             this project registers it as {} — the physical clone is shared",
            clone_dest.display(),
            other_project.as_str(),
            other_role.as_str(),
            this_role.as_str(),
        );
    }
}

/// Infer a clone URL from a local path by matching the first segment against
/// known registries.
///
/// For example, `github/owner/repo` matches the GitHub registry and produces
/// `https://github.com/owner/repo.git`.
fn infer_url_from_path(path: &str, registries: &[&dyn Registry]) -> Option<RepoUrl> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        return None;
    }

    // Parse into `RegistryName` at the boundary so the comparison runs
    // through the newtype rather than against its raw interior. (Audit A2.)
    let registry_name = crate::registry::RegistryName::new(segments[0]);
    let owner = segments[1];
    let repo = segments[2];

    let id = crate::registry::RepoId::new(owner, repo);

    for reg in registries {
        if reg.name() == &registry_name {
            return reg.clone_url(&id);
        }
    }

    None
}

/// Derive a local path for a URL no built-in registry matched, in the same
/// `{registry}/{owner}/{repo}` shape a matched registry would have produced.
/// The registry segment is the URL's own host (user-info and port stripped),
/// or `local` for `file://`, which has none. The bare `{owner}/{repo}` shape
/// a matched registry never produces is not a valid substitute here: it
/// collides with the workspace-root layout every other repo is written
/// under.
fn derive_local_path_from_url(url: &str) -> Option<String> {
    let (registry, path_str) = if let Some(rest) = url.strip_prefix("file://") {
        ("local".to_owned(), rest)
    } else if url.contains("://") {
        let rest = url.split("://").nth(1)?;
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?.split(':').next()?;
        if host.is_empty() {
            return None;
        }
        (host.to_owned(), path)
    } else {
        return None;
    };

    let trimmed = path_str.trim_end_matches('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }

    let repo = segments[segments.len() - 1];
    let owner = segments[segments.len() - 2];
    let repo = repo.strip_suffix(".git").unwrap_or(repo);

    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some(crate::registry::canonical_local_path(
        &registry, owner, repo,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{DomainRegistry, RegistryName};
    use std::path::PathBuf;

    fn github_reg() -> DomainRegistry {
        DomainRegistry {
            registry_name: RegistryName::new("github"),
            domain: "github.com".into(),
        }
    }

    fn gitlab_reg() -> DomainRegistry {
        DomainRegistry {
            registry_name: RegistryName::new("gitlab"),
            domain: "gitlab.com".into(),
        }
    }

    // -----------------------------------------------------------------------
    // infer_url_from_path
    // -----------------------------------------------------------------------

    #[test]
    fn infer_url_github_three_segments() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        let url = infer_url_from_path("github/owner/repo", &registries).unwrap();
        assert_eq!(url.to_string(), "https://github.com/owner/repo.git");
    }

    #[test]
    fn infer_url_gitlab_three_segments() {
        let gl = gitlab_reg();
        let registries: Vec<&dyn Registry> = vec![&gl];
        let url = infer_url_from_path("gitlab/org/project", &registries).unwrap();
        assert_eq!(url.to_string(), "https://gitlab.com/org/project.git");
    }

    #[test]
    fn infer_url_first_matching_registry_wins() {
        let gh = github_reg();
        let gl = gitlab_reg();
        let registries: Vec<&dyn Registry> = vec![&gh, &gl];
        let url = infer_url_from_path("github/alice/widgets", &registries).unwrap();
        assert_eq!(url.to_string(), "https://github.com/alice/widgets.git");
    }

    #[test]
    fn infer_url_unknown_registry_returns_none() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        assert!(infer_url_from_path("unknown/owner/repo", &registries).is_none());
    }

    #[test]
    fn infer_url_two_segments_returns_none() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        assert!(infer_url_from_path("owner/repo", &registries).is_none());
    }

    #[test]
    fn infer_url_single_segment_returns_none() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        assert!(infer_url_from_path("repo", &registries).is_none());
    }

    #[test]
    fn infer_url_empty_string_returns_none() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        assert!(infer_url_from_path("", &registries).is_none());
    }

    #[test]
    fn infer_url_empty_registries_returns_none() {
        let registries: Vec<&dyn Registry> = vec![];
        assert!(infer_url_from_path("github/owner/repo", &registries).is_none());
    }

    #[test]
    fn infer_url_extra_segments_uses_first_three() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        let url = infer_url_from_path("github/owner/repo/extra/path", &registries).unwrap();
        assert_eq!(url.to_string(), "https://github.com/owner/repo.git");
    }

    #[test]
    fn infer_url_leading_slash_ignored() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        let url = infer_url_from_path("/github/owner/repo", &registries).unwrap();
        assert_eq!(url.to_string(), "https://github.com/owner/repo.git");
    }

    // -----------------------------------------------------------------------
    // derive_local_path_from_url
    // -----------------------------------------------------------------------

    #[test]
    fn derive_path_from_file_url() {
        let url = format!(
            "file://{}",
            std::env::temp_dir().join("foo/bar/remote.git").display()
        );
        let path = derive_local_path_from_url(&url).unwrap();
        assert_eq!(path, PathBuf::from("local/bar/remote"));
    }

    #[test]
    fn derive_path_strips_git_suffix() {
        let path = derive_local_path_from_url("file:///srv/repos/owner/repo.git").unwrap();
        assert_eq!(path, PathBuf::from("local/owner/repo"));
    }

    #[test]
    fn derive_path_no_git_suffix() {
        let path = derive_local_path_from_url("file:///srv/repos/owner/repo").unwrap();
        assert_eq!(path, PathBuf::from("local/owner/repo"));
    }

    #[test]
    fn derive_path_https_url() {
        let path = derive_local_path_from_url("https://example.com/owner/repo.git").unwrap();
        assert_eq!(path, PathBuf::from("example.com/owner/repo"));
    }

    #[test]
    fn derive_path_trailing_slash() {
        let path = derive_local_path_from_url("file:///srv/repos/owner/repo/").unwrap();
        assert_eq!(path, PathBuf::from("local/owner/repo"));
    }

    #[test]
    fn derive_path_single_segment_returns_none() {
        assert!(derive_local_path_from_url("file:///repo").is_none());
    }

    #[test]
    fn derive_path_no_scheme_returns_none() {
        assert!(derive_local_path_from_url("/some/path").is_none());
    }

    #[test]
    fn derive_path_empty_returns_none() {
        assert!(derive_local_path_from_url("").is_none());
    }

    #[test]
    fn derive_path_unknown_host_gets_registry_segment() {
        // The bug this pins: a host no built-in registry recognises must
        // still produce the three-segment shape, not bare `owner/repo` —
        // the shape that used to escape into the workspace-root level.
        let path = derive_local_path_from_url("https://git.corp.example/team/repo.git").unwrap();
        assert_eq!(path, PathBuf::from("git.corp.example/team/repo"));
    }

    #[test]
    fn derive_path_strips_userinfo_and_port_from_host() {
        let path =
            derive_local_path_from_url("https://user@git.corp.example:8443/team/repo.git").unwrap();
        assert_eq!(path, PathBuf::from("git.corp.example/team/repo"));
    }

    #[test]
    fn derive_path_ssh_scheme_unknown_host_gets_registry_segment() {
        let path = derive_local_path_from_url("ssh://git@git.corp.example/team/repo.git").unwrap();
        assert_eq!(path, PathBuf::from("git.corp.example/team/repo"));
    }
}
