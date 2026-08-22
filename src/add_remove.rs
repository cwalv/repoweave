//! `rwv add` and `rwv remove` — manage repos in a project manifest.

use crate::activate::{activate_intent, activate_workweave_intent};
use crate::integration_runner::missing_active_members;
use crate::manifest::{Manifest, ProjectName, RepoEntry, RepoPath, RepoUrl, Role, VcsType};
use crate::refusal::{refusal, RefusalKind};
use crate::registry::{builtin_registries, CreationParamError, ParamMap, RegistryName, Upstream};
use crate::vcs::{vcs_for, EphemeralRefName, HeadAttachment, RefName, Vcs};
use crate::workspace::{project_dir, Checkout, WorkspaceContext};
use crate::workweave_index::RefRegistry;
use anyhow::Context;
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

    let repo_path = match crate::registry::placement_result(&parsed_url) {
        Ok(repo_path) => repo_path,
        Err(crate::registry::PlacementError::Invalid(e)) => return Err(e.into()),
        Err(crate::registry::PlacementError::NoMatch) => {
            let registries = builtin_registries();
            let names = registries
                .iter()
                .map(|r| r.name().as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(refusal(
                RefusalKind::NoMatchingRegistry,
                format!(
                    "unrecognized URL '{url}' — could not derive a local path \
                     (supported registries: {names})"
                ),
            ));
        }
    };

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
        let clone_url = crate::registry::resolve_to_clone_info(&parsed_url)?.url;
        vcs.clone_repo(&clone_url.to_string(), &dest)
            .with_context(|| format!("failed to clone '{}' into {}", clone_url, dest.display()))?;
    }

    let Some(remote_default) = vcs
        .remote_default_branch(&dest)
        .with_context(|| format!("failed to determine default branch for {}", dest.display()))?
    else {
        crate::refuse!(
            RefusalKind::NoRemoteDefaultBranch,
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
            refusal(
                RefusalKind::NoRemoteUrl,
                format!(
                    "'{}' has no {} remote to read a URL from",
                    clone_dir.display(),
                    vcs.conventional_remote_name()
                ),
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
        crate::refuse!(
            RefusalKind::NoRemoteDefaultBranch,
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
        crate::refuse!(
            RefusalKind::RepoNotInManifest,
            "path '{}' not found in manifest",
            repo_path.as_str()
        );
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
                    crate::refuse!(
                        RefusalKind::SharedCloneReferenced,
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
                crate::refuse!(
                    RefusalKind::StoreClaimsUnreadable,
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
            Err(e) => crate::refuse!(
                RefusalKind::StoreClaimsUnreadable,
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
    crate::refuse!(
        RefusalKind::StoreStillClaimed,
        "refusing to delete '{}': the store is still claimed:\n  {}\n\n\
         Deleting it would take every ref and object with it at once. Delete the \
         workweaves that hold these first (`rwv workweave <project> delete <name>`), \
         which removes their worktrees and retracts their receipts, then re-run.",
        repo_dir.display(),
        claims.join("\n  "),
    )
}

/// Parse an `rwv add --new` creation address into the registry it names and
/// the `(owner, repo)` prefix a three-segment shorthand fills, if any.
///
/// A bare registry name is one segment; the shorthand is exactly three
/// (`registry/owner/repo`, ruled to fill only that prefix — whatever else the
/// registry declares arrives via `--param`). Anything else does not look like
/// either spelling.
fn parse_creation_address(
    address: &str,
) -> anyhow::Result<(RegistryName, Option<(String, String)>)> {
    let segments: Vec<&str> = address.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [registry] => Ok((RegistryName::new(*registry), None)),
        [registry, owner, repo] => Ok((
            RegistryName::new(*registry),
            Some((owner.to_string(), repo.to_string())),
        )),
        _ => crate::refuse!(
            RefusalKind::MalformedRepoPath,
            "'{}' does not look like a valid creation address (expected a bare registry \
             name, or `registry/owner/repo`)",
            address
        ),
    }
}

/// The `missing-creation-param` refusal text: every name in `missing`, with
/// the `help` its own registry declares for it, and the one-flag repair for
/// each. Renders entirely from `registry.creation_params()` and `supplied`'s
/// own keys — nothing here is a literal per-registry sentence, which is what
/// keeps a registry that grows a parameter from needing an edit here too.
fn missing_creation_param_message(
    registry: &dyn crate::registry::Registry,
    missing: &[&'static str],
    supplied: &ParamMap,
) -> String {
    let need: Vec<String> = registry
        .creation_params()
        .iter()
        .filter(|p| missing.contains(&p.name))
        .map(|p| format!("{} ({})", p.name, p.help))
        .collect();
    let supplied_keys: Vec<&str> = supplied.keys().collect();
    let supplied_desc = if supplied_keys.is_empty() {
        "nothing".to_string()
    } else {
        supplied_keys.join(", ")
    };
    let flags: Vec<String> = missing
        .iter()
        .map(|n| format!("--param {n}=<value>"))
        .collect();
    format!(
        "'{}' needs {}; the address supplied {}. Re-run with {} added.",
        registry.name().as_str(),
        need.join(", "),
        supplied_desc,
        flags.join(" ")
    )
}

/// Insert `name=value` into `map`, refusing if `name` already has an entry —
/// the address's own shorthand, `--param`, and `--params-json` are three
/// spellings for the same map, and a name arriving through more than one of
/// them is a conflict rather than a precedence question.
fn insert_creation_param(map: &mut ParamMap, name: &str, value: String) -> anyhow::Result<()> {
    if map.insert(name, value).is_some() {
        crate::refuse!(
            RefusalKind::UnusableCreationParam,
            "creation parameter '{}' was supplied more than once — by the address \
             shorthand, `--param`, and `--params-json` each supply values into the same \
             map, and only one of them may name a given parameter",
            name
        );
    }
    Ok(())
}

/// Merge a creation address's own `(owner, repo)` prefix with `--param` and
/// `--params-json` into one [`ParamMap`].
fn build_creation_params(
    prefix: Option<(String, String)>,
    params: &[String],
    params_json: Option<&str>,
) -> anyhow::Result<ParamMap> {
    let mut map = ParamMap::new();

    if let Some((owner, repo)) = prefix {
        insert_creation_param(&mut map, "owner", owner)?;
        insert_creation_param(&mut map, "repo", repo)?;
    }

    for raw in params {
        let (name, value) = raw.split_once('=').ok_or_else(|| {
            refusal(
                RefusalKind::UnusableCreationParam,
                format!("'--param {raw}' is not NAME=VALUE"),
            )
        })?;
        insert_creation_param(&mut map, name, value.to_string())?;
    }

    if let Some(json) = params_json {
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
            refusal(
                RefusalKind::UnusableCreationParam,
                format!("--params-json is not valid JSON: {e}"),
            )
        })?;
        let obj = value.as_object().ok_or_else(|| {
            refusal(
                RefusalKind::UnusableCreationParam,
                "--params-json must be a JSON object of string values",
            )
        })?;
        for (name, v) in obj {
            let s = v.as_str().ok_or_else(|| {
                refusal(
                    RefusalKind::UnusableCreationParam,
                    format!(
                        "--params-json's '{name}' is not a string; creation parameters \
                         take string values only"
                    ),
                )
            })?;
            insert_creation_param(&mut map, name, s.to_string())?;
        }
    }

    Ok(map)
}

/// Execute `rwv add <creation-address> --new [--param NAME=VALUE]...
/// [--params-json JSON] [--role ROLE]`.
///
/// The address names a registry — a bare name, or a three-segment shorthand
/// that fills `owner` and `repo` for any registry (fork 7) — which turns a
/// filled-in parameter map into a [`registry::CreationPlan`](crate::registry::CreationPlan):
/// the URL the new member will be known by, and what, if anything, rwv must
/// create upstream before cloning the member from it. `placement` derives
/// the member's path from that URL; nothing here asserts one.
///
/// `ctx` is the already-resolved invocation context. Handlers must not
/// re-resolve.
pub fn run_add_new(
    address: &str,
    role: Role,
    params: &[String],
    params_json: Option<&str>,
    ctx: &WorkspaceContext,
) -> anyhow::Result<()> {
    // `rwv add` mints the manifest entry, so the backend is an input to the
    // verb rather than a lookup: one value feeds both the handle this verb
    // operates through and the `vcs_type` it records.
    let vcs_type = VcsType::Git;
    let vcs = vcs_for(vcs_type);
    let (project, project_dir) = find_project(ctx)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);

    let (registry_name, prefix) = parse_creation_address(address)?;

    let owned_registries = builtin_registries();
    let registry = owned_registries
        .iter()
        .find(|r| r.name() == &registry_name)
        .ok_or_else(|| {
            let names = crate::registry::builtin_registry_names();
            let known: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
            refusal(
                RefusalKind::UnknownRegistry,
                format!(
                    "'{}' is not a registry rwv can create through. Known registries: {}. \
                     To add a repo a registry already hosts, use `rwv add <url>` instead.",
                    registry_name.as_str(),
                    known.join(", ")
                ),
            )
        })?;

    let param_map = build_creation_params(prefix, params, params_json)?;

    let plan = match registry.plan_creation(&param_map) {
        Ok(plan) => plan,
        Err(CreationParamError::Missing(missing)) => {
            let message = missing_creation_param_message(registry.as_ref(), &missing, &param_map);
            crate::refuse!(RefusalKind::MissingCreationParam, "{}", message);
        }
        Err(CreationParamError::Unrecognized(name)) => {
            let declared: Vec<&str> = registry.creation_params().iter().map(|p| p.name).collect();
            crate::refuse!(
                RefusalKind::UnusableCreationParam,
                "'{}' does not accept a parameter named '{}'; it declares: {}",
                registry_name.as_str(),
                name,
                declared.join(", ")
            );
        }
    };

    let repo_path = crate::registry::placement(&plan.url)
        .expect("a CreationPlan's minted URL is always placeable");

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if let Some(existing) = manifest.get_entry(&repo_path) {
        if existing.url == plan.url {
            eprintln!(
                "Repository already exists in manifest at '{}'",
                repo_path.as_str()
            );
            return Ok(());
        }
        crate::refuse!(
            RefusalKind::OccupiedPlacement,
            "'{}' already maps to '{}' in the manifest; this creation would map it to \
             '{}' instead. Placement is a function of the identity, not of `--param root` \
             — two roots cannot both live at the same path.",
            repo_path.as_str(),
            existing.url,
            plan.url
        );
    }

    // Creation always lives at primary's canonical path (clones are global
    // infrastructure).
    let dest = ctx.primary_path().join(repo_path.as_path());

    // Warn when this repo path is already registered by another project with
    // a potentially different role. The physical init'd repo is shared; the
    // operator should know. This is not a refusal.
    warn_if_shared_clone(ctx.primary_path(), &project, &repo_path, &dest, role);

    match &plan.upstream {
        Upstream::InitBareAt(bare_path) => {
            let root = bare_path
                .parent()
                .and_then(|p| p.parent())
                .expect("InitBareAt is always root/owner/repo");
            if !root.is_dir() {
                crate::refuse!(
                    RefusalKind::UnusableCreationParam,
                    "the root '{}' does not exist; rwv creates '<root>/<owner>/<repo>' but \
                     not '<root>' itself — create it first",
                    root.display()
                );
            }
            let root_canon = root
                .canonicalize()
                .with_context(|| format!("failed to resolve root {}", root.display()))?;
            let weave_canon = ctx.primary_path().canonicalize().with_context(|| {
                format!(
                    "failed to resolve weave root {}",
                    ctx.primary_path().display()
                )
            })?;
            if root_canon.starts_with(&weave_canon) {
                crate::refuse!(
                    RefusalKind::UnusableCreationParam,
                    "the root '{}' is inside the weave at '{}'; the upstream it would \
                     create would then be walked, deletable, and reportable as a member \
                     of the very weave it backs — choose a root outside the weave",
                    root.display(),
                    weave_canon.display()
                );
            }

            if !bare_path.is_dir() {
                if let Some(parent) = bare_path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create directory {}", parent.display())
                    })?;
                }
                vcs.init_bare_repo(bare_path).with_context(|| {
                    format!("failed to init bare upstream at {}", bare_path.display())
                })?;
            }

            if dest.exists() {
                eprintln!(
                    "Directory already exists at '{}', skipping clone",
                    dest.display()
                );
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create directory {}", parent.display())
                    })?;
                }
                vcs.clone_repo(&plan.url.to_string(), &dest)
                    .with_context(|| {
                        format!("failed to clone '{}' into {}", plan.url, dest.display())
                    })?;
            }
        }
        Upstream::Named => {
            if dest.exists() {
                eprintln!(
                    "Directory already exists at '{}', skipping init",
                    dest.display()
                );
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create directory {}", parent.display())
                    })?;
                }
                vcs.init_repo(&dest)
                    .with_context(|| format!("failed to init repo at {}", dest.display()))?;
                vcs.add_remote(&dest, &plan.url.to_string())
                    .with_context(|| format!("failed to add the remote in {}", dest.display()))?;
            }
        }
    }

    let default_branch = match vcs.head_attachment(&dest)? {
        HeadAttachment::Unborn(u) => RefName::new(u.to_string()),
        HeadAttachment::Attached(a) => RefName::new(a.to_string()),
        HeadAttachment::Detached(_) => anyhow::bail!(
            "rwv add: repo at {} has a detached HEAD right after creation; \
             inspect with `git -C {} status` before retrying `rwv add {} --new`",
            dest.display(),
            dest.display(),
            address
        ),
    };

    let entry = RepoEntry {
        vcs_type,
        url: plan.url,
        version: default_branch,
        role,
    };
    manifest.insert_repo(repo_path.clone(), entry);

    // Write back the manifest (per-workspace state).
    manifest.write(&manifest_path)?;

    eprintln!("Added new repo '{}' to manifest", repo_path.as_str());

    // In a workweave, attempt to create a worktree at the workweave so the
    // new repo is materialized there. An unborn HEAD (both creation shapes
    // start unborn) means create_worktree_in_workweave silently skips until
    // the first commit lands upstream (operator can then `rwv sync`).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CreationPlan, ParamSpec, RepoId};

    // -----------------------------------------------------------------------
    // missing_creation_param_message (AC3's renderer pin)
    // -----------------------------------------------------------------------

    /// A registry whose parameter surface no built-in registry has, so a
    /// message-builder reading from a hardcoded literal instead of from
    /// `creation_params()` has something to disagree with.
    struct FixtureRegistry {
        registry_name: RegistryName,
    }

    const FIXTURE_PARAMS: &[ParamSpec] = &[ParamSpec {
        name: "warehouse",
        required: true,
        help: "the warehouse the widgets ship from",
    }];

    impl crate::registry::Registry for FixtureRegistry {
        fn name(&self) -> &RegistryName {
            &self.registry_name
        }
        fn matches(&self, _raw: &str) -> Option<RepoUrl> {
            None
        }
        fn clone_url(&self, _id: &RepoId) -> Option<RepoUrl> {
            None
        }
        fn creation_params(&self) -> &[ParamSpec] {
            FIXTURE_PARAMS
        }
        fn plan_creation(&self, _params: &ParamMap) -> Result<CreationPlan, CreationParamError> {
            unimplemented!("this fixture only exercises the message renderer")
        }
    }

    /// The missing-parameter refusal names the parameter and its `help` text
    /// exactly as a fixture registry — never seen by any hardcoded literal —
    /// declares them, with no edit to `missing_creation_param_message` itself.
    #[test]
    fn missing_creation_param_message_renders_from_the_registrys_own_declared_slice() {
        let fixture = FixtureRegistry {
            registry_name: RegistryName::new("fixture"),
        };
        let supplied = ParamMap::new();
        let message = missing_creation_param_message(&fixture, &["warehouse"], &supplied);
        assert!(
            message.contains("warehouse")
                && message.contains("the warehouse the widgets ship from"),
            "message must name the fixture's own declared parameter and help text: {message}"
        );
        assert!(
            message.contains("--param warehouse=<value>"),
            "message must print the one-flag repair: {message}"
        );
    }

    // -----------------------------------------------------------------------
    // parse_creation_address
    // -----------------------------------------------------------------------

    #[test]
    fn parse_creation_address_bare_registry_name() {
        let (registry, prefix) = parse_creation_address("local").unwrap();
        assert_eq!(registry.as_str(), "local");
        assert!(prefix.is_none());
    }

    #[test]
    fn parse_creation_address_three_segment_shorthand() {
        let (registry, prefix) = parse_creation_address("github/acme/fresh").unwrap();
        assert_eq!(registry.as_str(), "github");
        assert_eq!(prefix, Some(("acme".to_string(), "fresh".to_string())));
    }

    #[test]
    fn parse_creation_address_two_segments_is_malformed() {
        assert!(parse_creation_address("owner/repo").is_err());
    }

    #[test]
    fn parse_creation_address_four_segments_is_malformed() {
        assert!(parse_creation_address("github/owner/repo/extra").is_err());
    }

    #[test]
    fn parse_creation_address_empty_is_malformed() {
        assert!(parse_creation_address("").is_err());
    }

    // -----------------------------------------------------------------------
    // build_creation_params
    // -----------------------------------------------------------------------

    #[test]
    fn build_creation_params_merges_prefix_and_flags() {
        let map = build_creation_params(
            Some(("acme".to_string(), "fresh".to_string())),
            &["root=/srv/repos".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(map.get("owner"), Some("acme"));
        assert_eq!(map.get("repo"), Some("fresh"));
        assert_eq!(map.get("root"), Some("/srv/repos"));
    }

    #[test]
    fn build_creation_params_merges_json_spelling() {
        let map = build_creation_params(None, &[], Some(r#"{"owner": "acme", "repo": "fresh"}"#))
            .unwrap();
        assert_eq!(map.get("owner"), Some("acme"));
        assert_eq!(map.get("repo"), Some("fresh"));
    }

    #[test]
    fn build_creation_params_duplicate_across_prefix_and_param_refuses() {
        let err = build_creation_params(
            Some(("acme".to_string(), "fresh".to_string())),
            &["owner=other".to_string()],
            None,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("owner"));
    }

    #[test]
    fn build_creation_params_duplicate_across_param_and_json_refuses() {
        let err = build_creation_params(None, &["root=/a".to_string()], Some(r#"{"root": "/b"}"#))
            .unwrap_err();
        assert!(format!("{err:#}").contains("root"));
    }

    #[test]
    fn build_creation_params_non_string_json_value_refuses() {
        let err = build_creation_params(None, &[], Some(r#"{"root": 1}"#)).unwrap_err();
        assert!(format!("{err:#}").contains("root"));
    }

    #[test]
    fn build_creation_params_non_object_json_refuses() {
        assert!(build_creation_params(None, &[], Some("[1,2,3]")).is_err());
    }

    #[test]
    fn build_creation_params_malformed_param_flag_refuses() {
        assert!(build_creation_params(None, &["no-equals-sign".to_string()], None).is_err());
    }
}
