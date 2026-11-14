//! `rwv member add` and `rwv member remove` — manage repos in a project manifest.

use crate::activate::{activate_intent_for, ActivateOptions, ActivationFailure, RestorePoint};
use crate::integration_runner::missing_active_members;
use crate::manifest::{
    BelowError, LockFile, Manifest, ProjectName, RepoEntry, RepoPath, RepoUrl, Role,
    SubmoduleRecursion, UnspellableLocation, VcsType,
};
use crate::path_spelling::CanonicalPath;
use crate::refusal::{refusal, RefusalKind};
use crate::registry::{
    builtin_registries, recorded_remotes, resolve_to_clone_info, CloneUrl, CreationParamError,
    ParamKind, ParamMap, RecordedRemote, RegistryName, Upstream,
};
use crate::vcs::{
    vcs_for, HeadAttachment, RemoteBranchCopies, RemoteHandle, TrackingRef, UrlRemote, Vcs,
};
use crate::workspace::{project_dir, Checkout, WorkspaceContext};
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
pub(crate) fn find_project(ctx: &WorkspaceContext) -> anyhow::Result<(ProjectName, PathBuf)> {
    let name = ctx.require_active_project_on_disk()?.clone();
    let dir = project_dir(ctx.active_path(), &name);
    Ok((name, dir))
}

/// Create a worktree of `repo_path` in the workweave directory, pointing at
/// the canonical clone at `primary_root`, born at the canonical clone's HEAD
/// and settled for `role` ([`crate::workweave::materialize_member`]). A
/// symlink in the slot is replaced; a worktree already there is settled.
///
/// If the canonical clone has no HEAD (e.g. `rwv member add --new` produced an
/// empty `git init`), the worktree creation is skipped — `git worktree add`
/// against an unborn HEAD would fail. After the first commit, `rwv lock` pins
/// it and `rwv doctor --fix` creates the worktree.
#[allow(clippy::too_many_arguments)]
fn create_worktree_in_workweave(
    vcs: &dyn Vcs,
    primary_root: &Path,
    canonical_clone: &Path,
    workweave_dir: &Path,
    repo_path: &RepoPath,
    project: &ProjectName,
    workweave_name: &crate::manifest::WorkweaveName,
    role: Role,
    recurse_submodules: SubmoduleRecursion,
) -> anyhow::Result<()> {
    let dest = workweave_dir.join(repo_path.as_path());
    if !crate::workweave::is_symlinked_member(&dest) && dest.exists() {
        crate::settle::settle(vcs, &dest, role)?;
        return Ok(());
    }

    let start = match vcs.head_revision(canonical_clone) {
        Ok(start) => start,
        Err(_) => {
            eprintln!(
                "rwv member add: canonical clone at {} has no commits yet; \
                 skipping worktree creation in workweave \
                 (after the first commit, `rwv member pin {repo_path} HEAD` then \
                 `rwv doctor --fix` here create it)",
                canonical_clone.display()
            );
            return Ok(());
        }
    };

    let born = crate::workweave::materialize_member(
        vcs,
        primary_root,
        workweave_dir,
        project,
        workweave_name,
        repo_path,
        role,
        recurse_submodules,
        start.id().as_str(),
        crate::workweave::BranchPolicy::Restore,
    )
    .with_context(|| {
        format!(
            "failed to create worktree at {} from canonical clone {}",
            dest.display(),
            canonical_clone.display()
        )
    })?;
    if let Some(files) = born.files {
        eprintln!("Created the worktree for '{repo_path}' in this workweave ({files})");
    }
    Ok(())
}

/// Give the workspace `ctx` resolves to the member whose canonical clone is
/// `canonical`, settled for `role`: in a workweave, its worktree, with its
/// submodules as `recurse_submodules` asks; at primary, the canonical clone itself.
/// Nothing to do while the canonical clone is not on disk.
fn place_member(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    canonical: &Path,
    repo_path: &RepoPath,
    role: Role,
    recurse_submodules: SubmoduleRecursion,
) -> anyhow::Result<()> {
    if !canonical.exists() {
        return Ok(());
    }
    match &ctx.checkout {
        Checkout::Workweave {
            name, dir, project, ..
        } => create_worktree_in_workweave(
            vcs,
            ctx.primary_path(),
            canonical,
            dir.as_path(),
            repo_path,
            project,
            name.require(dir.as_path(), project)?,
            role,
            recurse_submodules,
        ),
        Checkout::Primary { .. } => crate::settle::settle(vcs, canonical, role).map(|_| ()),
    }
}

/// Write `edited` as `project`'s manifest and regenerate the integrations from
/// it, as one change; `None` regenerates from the manifest as it stands.
///
/// `rwv member add`/`rwv member remove` are **intent verbs**: they mutate `rwv.toml`, then
/// regenerate the integrations' managed/generated files so the new content can
/// be committed alongside the manifest change.
/// In a workweave we still regenerate (the workweave is a view onto the
/// project repo — symlinks write through to it) but skip install hooks; in
/// primary we run the full intent-mode activation.
///
/// A regeneration that fails before the install hooks puts `rwv.toml` and
/// every file it had rewritten back as they were, so the verb either lands
/// whole or not at all. The hooks are past that point: what `npm install` or
/// `cargo generate-lockfile` did to the disk has no inverse, so a failure there
/// leaves the change standing and says so.
///
/// `departing` names the checkouts the caller deletes once this lands; the
/// files are authored as they stand without them.
///
/// Regeneration is withheld when an active repo the manifest declares is not
/// on disk: the managed files are authored from the repos the run can see, so
/// over a partial member set they would be rewritten without the rest. The
/// manifest change still lands, and `rwv doctor --fix` regenerates once the
/// member set is whole.
pub(crate) fn settle_manifest(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    edited: Option<&Manifest>,
    departing: &[RepoPath],
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let root = ctx.active_path();
    let manifest_path = project_dir(root, project).join(Manifest::FILE_NAME);
    let on_disk = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;
    let manifest = edited.unwrap_or(&on_disk);
    let restore_point = RestorePoint::take(root, project, &[&on_disk, manifest])?;
    if let Some(edited) = edited {
        Manifest::edit_file(&manifest_path, |m| *m = edited.clone())?;
    }

    let missing = missing_active_members(root, manifest.iter_entries());
    if !missing.is_empty() {
        eprintln!(
            "warning: {} member repo(s) not on disk; managed files left unchanged:",
            missing.len()
        );
        for path in &missing {
            eprintln!("  - {path}");
        }
        eprintln!("run `rwv update` to materialize them, then `rwv doctor --fix` to regenerate.");
        return Ok(());
    }

    match activate_intent_for(project, ctx, opts, departing) {
        Ok(_) => {
            // Report only: the manifest change has already landed.
            for issue in crate::activate::member_incompatibilities(root, project, manifest) {
                eprintln!("[warning] {}: {}", issue.source, issue.message);
            }
            Ok(())
        }
        Err(ActivationFailure::BeforeHooks(e)) => {
            let unrestored = restore_point.restore();
            if unrestored.is_empty() {
                return Err(e.context(if edited.is_some() {
                    "rwv.toml is unchanged: the edit and every file its regeneration had \
                     rewritten were put back"
                } else {
                    "every file the regeneration had rewritten was put back"
                }));
            }
            let manifest_note = if unrestored
                .iter()
                .any(|held| held.is_file_at(&manifest_path))
            {
                "; rwv.toml still carries the change"
            } else {
                ""
            };
            Err(e.context(format!(
                "the regeneration failed, and these files could not be put back as they \
                 were: {}{manifest_note}",
                unrestored
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }
        Err(ActivationFailure::AtHooks(e)) if edited.is_some() => Err(e.context(
            "rwv.toml was updated and the integration files were regenerated from it; \
             only the install hooks did not complete",
        )),
        Err(ActivationFailure::AtHooks(e)) => Err(e),
    }
}

/// What a member `rwv member add` registers is given beyond where it comes
/// from.
#[derive(Debug, Clone, Copy)]
pub struct NewMemberSettings {
    pub role: Option<Role>,
    pub no_recurse_submodules: bool,
}

impl NewMemberSettings {
    fn role(self) -> Role {
        self.role.unwrap_or(Role::Owned)
    }

    fn recurse(self) -> SubmoduleRecursion {
        SubmoduleRecursion::from(!self.no_recurse_submodules)
    }

    /// Refuse a flag that disagrees with the entry the manifest already holds:
    /// an add of a registered member changes nothing in its entry, so a flag
    /// that asks for a change would be ignored.
    fn refuse_if_disagrees(self, existing: &RepoEntry, repo_path: &RepoPath) -> anyhow::Result<()> {
        if let Some(role) = self.role.filter(|role| *role != existing.role) {
            crate::refuse!(
                RefusalKind::InapplicableFlag,
                "rwv member add: '{repo_path}' is already a member with the role {}; --role {role} \
                 would change nothing here. `rwv member set-role {repo_path} {role}` changes it",
                existing.role
            );
        }
        if self.no_recurse_submodules && existing.recurse_submodules != SubmoduleRecursion::Skip {
            crate::refuse!(
                RefusalKind::InapplicableFlag,
                "rwv member add: '{repo_path}' is already a member that recurses into its \
                 submodules; --no-recurse-submodules would change nothing here. `rwv member \
                 set-recurse-submodules {repo_path} false` changes it"
            );
        }
        Ok(())
    }
}

/// Execute `rwv member add URL [--role=ROLE]`.
///
/// `ctx` is the already-resolved invocation context (with `--project`
/// baked in when passed). Handlers must not re-resolve.
pub fn run_add(
    url: &str,
    settings: NewMemberSettings,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let (role, recurse_submodules) = (settings.role(), settings.recurse());
    let parsed_url: RepoUrl = url.parse()?;
    let repo_path = match crate::registry::placement_result(&parsed_url) {
        Ok(repo_path) => repo_path,
        Err(crate::registry::PlacementError::Invalid(e)) => return Err(e.into()),
        Err(crate::registry::PlacementError::NoMatch) if !parsed_url.is_url() => {
            return Err(crate::registry::unresolvable_source(
                Some("rwv member add"),
                url,
                Some(format!(
                    "To register the clone already at that path under the weave root: \
                     `rwv member add --adopt {url}`."
                )),
            ));
        }
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
    let clone_url = resolve_to_clone_info(&parsed_url)?.url;
    // `rwv member add` mints the manifest entry, so the backend is an input to the
    // verb rather than a lookup: one value feeds both the handle this verb
    // operates through and the `vcs_type` it records.
    let vcs_type = VcsType::Git;
    let vcs = vcs_for(vcs_type);
    let (project, project_dir) = find_project(ctx)?;
    crate::diagnosis::settle_project_drift(&project, &project_dir)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    // Clone the repo if it doesn't exist on disk. The clone always lives
    // at primary's canonical path — the canonical store, in tier-0
    // topology terms; workweaves link into it via `git worktree` rather
    // than holding their own clones (see
    // `docs/explanation/joints/clone-topology.md`).
    let dest = ctx.primary_path().join(repo_path.as_path());

    if let Some(existing) = manifest.get_entry(&repo_path) {
        settings.refuse_if_disagrees(existing, &repo_path)?;
        let declared = resolve_to_clone_info(&existing.url)?.url;
        if !declared.names_same_remote(&clone_url) {
            let remove = format!("`rwv member remove {repo_path} --delete`");
            let replace = match ctx.checkout {
                Checkout::Workweave { .. } if dest.exists() => {
                    format!("{remove} here and then in the primary weave, which holds the clone")
                }
                _ => remove,
            };
            crate::refuse!(
                RefusalKind::OccupiedPlacement,
                "'{}' already maps to '{}' in the manifest; this add would map it to \
                 '{}' instead. Re-run with the manifest's URL to finish that entry. If \
                 replacing it is what's intended, first run {replace}.",
                repo_path.as_str(),
                existing.url,
                url,
            );
        }
        refuse_non_clone_at_placements(
            vcs.as_ref(),
            ctx,
            &repo_path,
            &format!("rwv member add {url}"),
        )?;
        announce_already_registered(&repo_path);
        if !dest.exists() {
            warn_if_shared_clone(
                ctx.primary_path(),
                &project,
                &repo_path,
                &dest,
                existing.role,
            );
            clone_into(vcs.as_ref(), &declared, &dest)?;
        }
        crate::workweave::fetch_member_submodules(
            vcs.as_ref(),
            &dest,
            &repo_path,
            existing.role,
            existing.recurse_submodules,
            "rwv member add: ",
        );
        place_member(
            vcs.as_ref(),
            ctx,
            &dest,
            &repo_path,
            existing.role,
            existing.recurse_submodules,
        )?;
        return settle_registered(
            ctx,
            &project,
            &project_dir,
            &repo_path,
            &dest,
            vcs.as_ref(),
            opts,
        );
    }

    refuse_non_clone_at_placements(
        vcs.as_ref(),
        ctx,
        &repo_path,
        &format!("rwv member add {url}"),
    )?;
    if vcs.is_repo(&dest) {
        refuse_foreign_clone_at_placement(vcs.as_ref(), &clone_url, &dest, &repo_path)?;
    }

    judge_commit_of_add(&project_dir)?;

    // Warn when this clone path is already registered by another project.
    // The physical clone is shared; the operator should know. This is not a
    // refusal — sharing is legitimate and supported.
    warn_if_shared_clone(ctx.primary_path(), &project, &repo_path, &dest, role);
    let mut left = AddLeftovers::default();
    let added = (|| -> anyhow::Result<()> {
        if dest.exists() {
            eprintln!(
                "Directory already exists at '{}', skipping clone",
                dest.display()
            );
        } else {
            clone_into(vcs.as_ref(), &clone_url, &dest)?;
            left.clone = Some(dest.clone());
        }

        let url_remote = present_url_remote(vcs.as_ref(), &dest, &clone_url)?;
        let Some(remote_default) =
            vcs.remote_default_branch(&dest, &url_remote)
                .with_context(|| {
                    format!("failed to determine default branch for {}", dest.display())
                })?
        else {
            crate::refuse!(
                RefusalKind::NoRemoteDefaultBranch,
                "rwv member add: '{}' at {}: {}, then re-run `rwv member add {}`",
                repo_path.as_str(),
                dest.display(),
                vcs.clone_default_branch_repair_hint(
                    &dest,
                    &url_remote,
                    RemoteBranchCopies::JustCloned
                )?,
                url
            );
        };
        let branch = declared_branch(
            TrackingRef::try_from(&remote_default),
            vcs.as_ref(),
            &repo_path,
            &format!(
                "the default branch of the remote cloned at {}",
                dest.display()
            ),
            || untrackable_remote_default(&repo_path, &dest),
        )?;

        // Add entry to manifest.
        let entry = RepoEntry {
            vcs_type,
            url: parsed_url,
            branch,
            role,
            recurse_submodules,
            history: None,
        };
        manifest.insert_repo(repo_path.clone(), entry);
        crate::workweave::fetch_member_submodules(
            vcs.as_ref(),
            &dest,
            &repo_path,
            role,
            recurse_submodules,
            "rwv member add: ",
        );

        // In a workweave, also create a worktree at the workweave so the new
        // repo is materialized there. Before the manifest names it, because the
        // regeneration that follows authors only from members it finds on disk.
        place_member_noting(
            vcs.as_ref(),
            ctx,
            &dest,
            &repo_path,
            role,
            recurse_submodules,
            &mut left,
        )?;

        // Per-workspace state: the workweave's own copy when CWD is in one,
        // primary's otherwise.
        settle_and_commit(
            ctx,
            &project,
            &project_dir,
            &manifest,
            &repo_path,
            &dest,
            vcs.as_ref(),
            opts,
        )
    })();
    added.map_err(|e| {
        left.refused(
            e,
            &manifest_path,
            &repo_path,
            &format!("rwv member add {url}"),
        )
    })?;

    eprintln!("Added '{}' to manifest", repo_path.as_str());
    Ok(())
}

/// The first pin of a member just added: the commit its clone's HEAD is at,
/// where HEAD sits on the declared branch or that branch's upstream reaches
/// it; none where HEAD is unborn or holds commits only the clone has.
pub(crate) fn first_pin(
    vcs: &dyn Vcs,
    clone: &Path,
    entry: &RepoEntry,
) -> anyhow::Result<Option<crate::vcs::ResolvedRevisionId>> {
    let attached = match vcs.head_attachment(clone)? {
        HeadAttachment::Unborn(_) => return Ok(None),
        HeadAttachment::Attached(attached) => Some(attached),
        HeadAttachment::Detached(_) => None,
    };
    let head = vcs.head_revision(clone)?;
    let declared = crate::vcs::RawRefName::new(entry.branch.local_counterpart().as_str());
    let sits_on_declared = attached
        .is_some_and(|attached| attached.is_named(&crate::vcs::LocalRefName::of_branch(&declared)));
    let reached_by_upstream = vcs
        .remote_copies(clone, &declared)?
        .iter()
        .any(|copy| copy.tip == head || vcs.is_ancestor(clone, &head, &copy.tip).unwrap_or(false));
    Ok((sits_on_declared || reached_by_upstream).then_some(head))
}

/// Refuse, before the add changes anything, a project with no repo to commit
/// the addition to.
fn judge_commit_of_add(project_dir: &Path) -> anyhow::Result<()> {
    if !crate::vcs::project_vcs().is_repo(project_dir) {
        return Err(crate::untokened!(
            Owed,
            "{} is not a repository, so `rwv member add` has nowhere to commit the member and its \
         first pin; `rwv init` makes a project repo",
            project_dir.display()
        ));
    }
    Ok(())
}

/// Settle the manifest that adds `repo_path`, then commit the addition with
/// the member's first pin: the manifest, the integration files regenerated
/// from it and `rwv.lock`, in one commit.
///
/// A settle that failed after the manifest landed (an install hook) still
/// leaves the member added, so its commit is made before that failure is
/// reported.
#[allow(clippy::too_many_arguments)]
fn settle_and_commit(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    project_dir: &Path,
    manifest: &Manifest,
    repo_path: &RepoPath,
    canonical: &Path,
    vcs: &dyn Vcs,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let settled = settle_manifest(ctx, project, Some(manifest), &[], opts);
    let on_disk = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME));
    let Some(on_disk) = on_disk.ok().filter(|m| m.contains_repo(repo_path)) else {
        return settled;
    };
    let committed = commit_addition(
        ctx,
        project,
        project_dir,
        &on_disk,
        repo_path,
        canonical,
        vcs,
    );
    settled.and(committed)
}

/// Settle a member the manifest already names, then give it the first pin it
/// lacks, in the commit that carries the entry: a hand edit that added the
/// member is finished by the verb that adds it.
#[allow(clippy::too_many_arguments)]
fn settle_registered(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    project_dir: &Path,
    repo_path: &RepoPath,
    canonical: &Path,
    vcs: &dyn Vcs,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let settled = settle_manifest(ctx, project, None, &[], opts);
    let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let pinned = crate::lock::read_old_lock(&LockFile::path_in(project_dir), "member add")
        .is_some_and(|lock| lock.contains_repo(repo_path));
    if pinned || !canonical.exists() || manifest.get_entry(repo_path).is_none() {
        return settled;
    }
    let committed = commit_addition(
        ctx,
        project,
        project_dir,
        &manifest,
        repo_path,
        canonical,
        vcs,
    );
    settled.and(committed)
}

fn commit_addition(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    project_dir: &Path,
    manifest: &Manifest,
    repo_path: &RepoPath,
    canonical: &Path,
    vcs: &dyn Vcs,
) -> anyhow::Result<()> {
    let entry = manifest
        .get_entry(repo_path)
        .expect("the manifest names the member just added");
    let pin = first_pin(vcs, canonical, entry)?;
    let lock_path = LockFile::path_in(project_dir);
    let old = crate::lock::read_old_lock(&lock_path, "member add");
    let new = crate::lock::with_pins(manifest, old.as_ref(), pin.iter().map(|p| (repo_path, p)));
    crate::lock::write_lock_in(&new, project_dir)?;
    let message = match &pin {
        Some(pin) => format!(
            "member add {repo_path}\n\n  - {repo_path}: {}",
            pin.id().short()
        ),
        None => format!("member add {repo_path}\n\n  - {repo_path}: no pin"),
    };
    let authored = crate::manifest_commit::authored_across(ctx, project, &[manifest]);
    let member = repo_path.clone();
    let added = entry.clone();
    let edit_manifest = move |committed: &mut Manifest| {
        committed.insert_repo(member.clone(), added.clone());
    };
    let pinned = new.get_entry(repo_path).cloned();
    let member = repo_path.clone();
    let edit_lock = move |lock: &mut LockFile| {
        if let Some(pinned) = &pinned {
            lock.insert_entry(member.clone(), pinned.clone());
        }
    };
    crate::manifest_commit::commit(&crate::manifest_commit::MemberCommit {
        project,
        project_dir,
        edit_manifest: &edit_manifest,
        edit_lock: Some(&edit_lock),
        authored: &authored,
        message: &message,
        naming: crate::repin::Naming::Unnamed,
    })?;
    match &pin {
        Some(pin) => eprintln!("Pinned '{repo_path}' at {}", pin.id().short()),
        None => {
            let mut report = crate::diagnosis::Report::default();
            report.add(
                crate::diagnosis::State::PinNone
                    .of(repo_path)
                    .member()
                    .checkout(canonical)
                    .bind("<m>", repo_path.as_str())
                    .judge(vcs),
            );
            eprint!("{}", report.render("member add"));
        }
    }
    Ok(())
}

/// Anything at a placement this add materializes into — the primary's, and
/// from a workweave the workweave's own — that is not a clone: a directory,
/// empty or not, or a file. It has no remote to compare, so the add can
/// neither register it nor place a checkout over it.
fn refuse_non_clone_at_placements(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    repo_path: &RepoPath,
    retry: &str,
) -> anyhow::Result<()> {
    let placements = [ctx.primary_path(), ctx.active_path()];
    for at in placements.map(|root| root.join(repo_path.as_path())) {
        refuse_non_clone_at(vcs, &at, repo_path, retry)?;
    }
    Ok(())
}

fn refuse_non_clone_at(
    vcs: &dyn Vcs,
    at: &Path,
    repo_path: &RepoPath,
    retry: &str,
) -> anyhow::Result<()> {
    if !at.exists() || vcs.is_repo(at) {
        return Ok(());
    }
    let found = if at.is_dir() {
        "a directory that is not a clone"
    } else {
        "a file"
    };
    crate::refuse!(
        RefusalKind::NonCloneAtPlacement,
        "rwv member add: the placement '{}' ({}) holds {found}, which can be neither registered \
         nor have a checkout placed over it. Move it aside and re-run `{retry}`.",
        repo_path.as_str(),
        at.display(),
    )
}

/// A clone already at the placement stands in for the one this add would make
/// only when one of its remotes names the same repo. Any other clone is
/// registered through `--adopt`, under its own remote.
fn refuse_foreign_clone_at_placement(
    vcs: &dyn Vcs,
    clone_url: &CloneUrl,
    dest: &Path,
    repo_path: &RepoPath,
) -> anyhow::Result<()> {
    let recorded = recorded_remotes(vcs, dest)
        .with_context(|| format!("could not determine the remotes of '{}'", dest.display()))?;
    if recorded.iter().any(
        |(_, remote)| matches!(remote, RecordedRemote::Url(r) if r.names_same_remote(clone_url)),
    ) {
        return Ok(());
    }
    let found = match recorded.as_slice() {
        [] => "a clone with no remote".to_owned(),
        [(_, only)] => format!("a clone of '{only}'"),
        several => format!(
            "a clone of {}",
            several
                .iter()
                .map(|(_, r)| format!("'{r}'"))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
    };
    crate::refuse!(
        RefusalKind::ForeignCloneAtPlacement,
        "rwv member add: '{path}' already holds {found}, not '{clone_url}'. To register that \
         clone under the URL it records: `rwv member add --adopt {path}`. To add '{clone_url}', \
         move the existing clone aside first.",
        path = repo_path.as_str(),
    )
}

/// `rwv member add --adopt <path>`: register the clone the absolute `path` names. Its
/// member path is its position inside whichever weave root contains it, and
/// the clone itself is always the canonical one under the primary root.
pub fn run_add_adopt(
    path: &Path,
    settings: NewMemberSettings,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let (role, recurse_submodules) = (settings.role(), settings.recurse());
    let vcs_type = VcsType::Git;
    let vcs = vcs_for(vcs_type);
    let (project, project_dir) = find_project(ctx)?;
    crate::diagnosis::settle_project_drift(&project, &project_dir)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let (repo_path, candidate) = adopt_placement(vcs.as_ref(), ctx, path)?;
    if !vcs.is_repo(&candidate) {
        crate::refuse!(
            RefusalKind::NoCloneToAdopt,
            "rwv member add --adopt: no clone at '{}'. --adopt registers a clone already under \
             the weave root; to clone from a remote instead, drop --adopt and give its URL \
             (`file://` for a local one).",
            candidate.display()
        );
    }
    judge_commit_of_add(&project_dir)?;
    warn_if_shared_clone(ctx.primary_path(), &project, &repo_path, &candidate, role);
    let registration = run_add_from_local_path(
        vcs.as_ref(),
        vcs_type,
        &repo_path,
        path,
        &candidate,
        settings,
        &manifest_path,
    )?;
    // Adopting doesn't clone, but the clone still has to hold the permissions
    // of the role the manifest gives it, and a workweave still needs its
    // worktree, as on the URL path.
    let canonical = ctx.primary_path().join(repo_path.as_path());
    let (role, recurse_submodules) = match &registration {
        Some((manifest, _)) => manifest
            .get_entry(&repo_path)
            .map(|e| (e.role, e.recurse_submodules)),
        None => Manifest::from_path(&manifest_path)?
            .get_entry(&repo_path)
            .map(|e| (e.role, e.recurse_submodules)),
    }
    .unwrap_or((role, recurse_submodules));
    let mut left = AddLeftovers::default();
    let added = (|| -> anyhow::Result<()> {
        place_member_noting(
            vcs.as_ref(),
            ctx,
            &canonical,
            &repo_path,
            role,
            recurse_submodules,
            &mut left,
        )?;
        match registration.as_ref().map(|(manifest, _)| manifest) {
            Some(manifest) => settle_and_commit(
                ctx,
                &project,
                &project_dir,
                manifest,
                &repo_path,
                &canonical,
                vcs.as_ref(),
                opts,
            ),
            None => settle_registered(
                ctx,
                &project,
                &project_dir,
                &repo_path,
                &canonical,
                vcs.as_ref(),
                opts,
            ),
        }
    })();
    added.map_err(|e| {
        left.refused(
            e,
            &manifest_path,
            &repo_path,
            &format!("rwv member add --adopt {}", path.display()),
        )
    })?;
    if let Some((_, origin_url)) = registration {
        eprintln!(
            "Added '{}' to manifest (url: {})",
            repo_path.as_str(),
            origin_url
        );
    }
    Ok(())
}

/// The member path an `--adopt` argument names, and the canonical clone at
/// that member path under the primary root.
///
/// The weave roots in play are the primary and, from a workweave, that
/// workweave. The innermost one containing the location `path` resolves to
/// decides the member path, so a `..` after a symlink climbs from where the
/// symlink points, as it does for the OS. Named inside a workweave, a clone
/// already there that is not a worktree of the primary's clone refuses:
/// adopting would register the primary's copy while the operator pointed at
/// another.
fn adopt_placement(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    path: &Path,
) -> anyhow::Result<(RepoPath, PathBuf)> {
    let named = CanonicalPath::of(path);
    let primary = ctx.primary_root();
    let workweave = match &ctx.checkout {
        Checkout::Workweave { dir, .. } => Some(dir),
        Checkout::Primary { .. } => None,
    };
    let placed = [Some(primary), workweave]
        .into_iter()
        .flatten()
        .filter_map(
            |root| match RepoPath::below(root.as_path(), named.as_path()) {
                Ok(repo_path) => Some((root, Ok(repo_path))),
                Err(BelowError::Unspellable(why)) => Some((root, Err(why))),
                Err(BelowError::NotBelow) => None,
            },
        )
        .max_by_key(|(root, _)| root.as_path().components().count());
    let Some((root, spelled)) = placed else {
        let roots = match workweave {
            Some(ww) => format!(
                "this workweave ('{}') or the primary weave ('{}')",
                ww.as_path().display(),
                primary.as_path().display()
            ),
            None => format!("the weave root '{}'", primary.as_path().display()),
        };
        crate::refuse!(
            RefusalKind::AdoptPathOutsideWeave,
            "rwv member add --adopt: '{}' is not under {roots}. A relative path is read from the \
             directory rwv runs in (or -C/-w), and a symlink in it is followed; name the clone \
             by a path inside the weave.",
            named.as_path().display()
        );
    };
    let repo_path = match spelled {
        Ok(repo_path) => repo_path,
        Err(UnspellableLocation::Rejected(e)) => return Err(e.into()),
        Err(why @ (UnspellableLocation::Backslash(_) | UnspellableLocation::NotUnicode(_))) => {
            crate::refuse!(
                RefusalKind::UnspellableDirectoryName,
                "rwv member add --adopt: no member path can name '{}': {why}. Rename the directory, \
                 then adopt the clone by its new path; rwv does not choose the name.",
                named.as_path().display()
            )
        }
    };
    let canonical = primary.as_path().join(repo_path.as_path());
    if let Some(ww) = workweave.filter(|ww| *ww == root) {
        let checkout = ww.as_path().join(repo_path.as_path());
        if vcs.is_repo(&checkout) && !shares_store(vcs, &checkout, &canonical) {
            crate::refuse!(
                RefusalKind::ForeignCloneInWorkweave,
                "rwv member add --adopt: '{}' is a clone of its own, not a worktree of the primary's \
                 clone at '{}'. --adopt registers the clone under the primary weave root; move \
                 '{}' there, or name '{}' directly.",
                checkout.display(),
                canonical.display(),
                checkout.display(),
                canonical.display()
            );
        }
        refuse_non_clone_at(
            vcs,
            &checkout,
            &repo_path,
            &format!("rwv member add --adopt {}", path.display()),
        )?;
    }
    Ok((repo_path, canonical))
}

fn shares_store(vcs: &dyn Vcs, a: &Path, b: &Path) -> bool {
    let store = |p: &Path| {
        vcs.store_of(p)
            .and_then(|s| CanonicalPath::existing(&s).ok())
    };
    matches!((store(a), store(b)), (Some(x), Some(y)) if x == y)
}

/// [`place_member`], noting in `left` a workweave worktree it created.
fn place_member_noting(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    canonical: &Path,
    repo_path: &RepoPath,
    role: Role,
    recurse_submodules: SubmoduleRecursion,
    left: &mut AddLeftovers,
) -> anyhow::Result<()> {
    let checkout = ctx.active_path().join(repo_path.as_path());
    let fresh = matches!(ctx.checkout, Checkout::Workweave { .. }) && !checkout.exists();
    let placed = place_member(vcs, ctx, canonical, repo_path, role, recurse_submodules);
    if fresh && checkout.exists() {
        left.worktree = Some(checkout);
    }
    placed
}

enum Made {
    Dir(PathBuf),
    Repo(PathBuf),
}

/// What an add created on its way to a refusal. A creation's own
/// directories and repositories hold nothing but what the add put there, so
/// they are taken back; a clone fetched from an existing remote, and a
/// workweave worktree, are kept for a re-run to reuse, and named.
#[derive(Default)]
struct AddLeftovers {
    made: Vec<Made>,
    clone: Option<PathBuf>,
    worktree: Option<PathBuf>,
}

impl AddLeftovers {
    fn make_dir_all(&mut self, dir: &Path) -> anyhow::Result<()> {
        let missing: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|d| !d.exists())
            .map(Path::to_path_buf)
            .collect();
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory {}", dir.display()))?;
        self.made.extend(missing.into_iter().rev().map(Made::Dir));
        Ok(())
    }

    /// `err`, accounting for what this add left behind. Nothing is left
    /// behind once the manifest names the member: past the install hooks the
    /// add stands, and what it created is the member.
    fn refused(
        self,
        err: anyhow::Error,
        manifest_path: &Path,
        repo_path: &RepoPath,
        rerun: &str,
    ) -> anyhow::Error {
        let registered = Manifest::from_path(manifest_path)
            .map(|m| m.contains_repo(repo_path))
            .unwrap_or(false);
        if registered {
            return err;
        }
        let mut lines = Vec::new();
        for made in self.made.into_iter().rev() {
            match made {
                Made::Repo(repo) if repo.exists() => match std::fs::remove_dir_all(&repo) {
                    Ok(()) => {
                        lines.push(format!("removed {}, which it had created", repo.display()))
                    }
                    Err(e) => lines.push(format!(
                        "{} was created by it and could not be removed ({e:#}); delete it by hand",
                        repo.display()
                    )),
                },
                Made::Repo(_) => {}
                Made::Dir(dir) => {
                    let _ = std::fs::remove_dir(dir);
                }
            }
        }
        if let Some(worktree) = &self.worktree {
            lines.push(format!(
                "kept the worktree it made at {}: once the cause below is fixed, \
                 `{rerun}` reuses it, or `rwv member remove {repo_path} --delete` in this \
                 workweave removes it",
                worktree.display()
            ));
        }
        if let Some(clone) = &self.clone {
            let after_worktree = if self.worktree.is_some() {
                " once that worktree is removed"
            } else {
                ""
            };
            lines.push(format!(
                "kept the clone it made at {}: once the cause below is fixed, \
                 `{rerun}` reuses it, or `rwv member remove {repo_path} --delete` in the primary \
                 weave removes it{after_worktree}",
                clone.display()
            ));
        }
        if lines.is_empty() {
            return err;
        }
        err.context(format!(
            "the add did not complete and rwv.toml does not list '{repo_path}'; this add\n  {}",
            lines.join("\n  ")
        ))
    }
}

/// The branch `rwv member add` declares for `repo_path`, from `parsed`; one that
/// reads as a revision is refused as that repo's `name` state, `whose` saying
/// where the name came from, and any other unusable name under `context`.
fn declared_branch(
    parsed: Result<TrackingRef, crate::naming::RefNameError>,
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    whose: &str,
    context: impl FnOnce() -> String,
) -> anyhow::Result<TrackingRef> {
    use crate::naming::RefNameError as E;
    let (name, shape) = match parsed {
        Ok(declared) => return Ok(declared),
        Err(E::ShaShaped(name)) => (name, "a commit id"),
        Err(E::TagShaped(name)) => (name, "a release tag"),
        Err(other) => return Err(anyhow::Error::new(other).context(context())),
    };
    Err(crate::diagnosis::State::NameARevision
        .of(repo_path)
        .subject(&name)
        .listing([format!(
            "{whose} reads as {shape}, so there is no branch to track"
        )])
        .judge(vcs)
        .refused())
}

fn untrackable_remote_default(repo_path: &RepoPath, clone: &Path) -> String {
    format!(
        "rwv member add: '{}' at {}: the remote's default branch cannot be its declared `branch`",
        repo_path.as_str(),
        clone.display(),
    )
}

/// The remote of the clone at `dest` that has `url`.
fn present_url_remote(vcs: &dyn Vcs, dest: &Path, url: &CloneUrl) -> anyhow::Result<RemoteHandle> {
    match vcs.url_remote(dest, url)? {
        UrlRemote::Present(remote) => Ok(remote),
        UrlRemote::Absent(_) | UrlRemote::NameTaken(_) => Err(refusal(
            RefusalKind::NoRemoteUrl,
            format!("'{}' has no remote with the url '{url}'", dest.display()),
        )),
    }
}

fn clone_into(vcs: &dyn Vcs, clone_url: &CloneUrl, dest: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    vcs.clone_repo(clone_url, dest)
        .with_context(|| format!("failed to clone '{}' into {}", clone_url, dest.display()))?;
    Ok(())
}

/// Re-running `add` for a member the manifest already names changes nothing
/// in it, and still re-runs the regeneration, which is what repairs a weave a
/// previous run left short.
fn announce_already_registered(repo_path: &RepoPath) {
    eprintln!(
        "Repository already exists in manifest at '{}'; re-running its activation",
        repo_path.as_str()
    );
}

/// Register the clone at `clone_dir` as `repo_path`, inferring the URL by
/// reading the clone's `origin` remote.
///
/// Returns the manifest with the member added and the remote it was recorded
/// under, unwritten, or `None` when the manifest already names it.
fn run_add_from_local_path(
    vcs: &dyn Vcs,
    vcs_type: VcsType,
    repo_path: &RepoPath,
    adopted: &Path,
    clone_dir: &Path,
    settings: NewMemberSettings,
    manifest_path: &Path,
) -> anyhow::Result<Option<(Manifest, CloneUrl)>> {
    let (role, recurse_submodules) = (settings.role(), settings.recurse());
    let remotes = recorded_remotes(vcs, clone_dir).with_context(|| {
        format!(
            "could not determine the remotes of '{}'",
            clone_dir.display()
        )
    })?;
    let (url_remote, recorded) = match remotes.as_slice() {
        [] => crate::refuse!(
            RefusalKind::NoRemoteUrl,
            "'{}' has no remote to read a URL from",
            clone_dir.display()
        ),
        [only] => only.clone(),
        several => crate::refuse!(
            RefusalKind::NoRemoteUrl,
            "'{dir}' has {count} remotes ({listed}), and `--adopt` records the url of a \
             clone's only remote, which is a choice among these. Remove the remotes that are \
             not the member's, or, for a clone at the placement of one of these urls, name \
             that url: `rwv member add <url>`",
            dir = clone_dir.display(),
            count = several.len(),
            listed = several
                .iter()
                .map(|(remote, url)| format!("{remote} = {url}"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    };
    let recorded = match recorded {
        RecordedRemote::Url(url) => url,
        RecordedRemote::RelativePath(path) => crate::refuse!(
            RefusalKind::NoRemoteUrl,
            "'{dir}' records its {remote} remote as the relative path '{path}', which git \
             reads from each checkout's own directory, so it names no one URL to record; \
             give it an absolute URL (`git -C {dir} remote set-url {remote} <url>`) and \
             re-run `rwv member add --adopt {adopted}`",
            dir = clone_dir.display(),
            remote = url_remote,
            adopted = adopted.display(),
        ),
    };

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if let Some(existing) = manifest.get_entry(repo_path) {
        settings.refuse_if_disagrees(existing, repo_path)?;
        announce_already_registered(repo_path);
        return Ok(None);
    }

    let Some(remote_default) = vcs
        .remote_default_branch(clone_dir, &url_remote)
        .with_context(|| {
            format!(
                "failed to determine default branch for {}",
                clone_dir.display()
            )
        })?
    else {
        crate::refuse!(
            RefusalKind::NoRemoteDefaultBranch,
            "rwv member add: '{}' at {}: {}, then re-run `rwv member add --adopt {}`",
            repo_path.as_str(),
            clone_dir.display(),
            vcs.clone_default_branch_repair_hint(
                clone_dir,
                &url_remote,
                RemoteBranchCopies::AsLastFetched
            )?,
            adopted.display()
        );
    };
    let branch = declared_branch(
        TrackingRef::try_from(&remote_default),
        vcs,
        repo_path,
        &format!(
            "the default branch of the remote of the clone at {}",
            clone_dir.display()
        ),
        || untrackable_remote_default(repo_path, clone_dir),
    )?;

    let entry = RepoEntry {
        vcs_type,
        url: RepoUrl::from(&recorded),
        branch,
        role,
        recurse_submodules,
        history: None,
    };
    manifest.insert_repo(repo_path.clone(), entry);
    Ok(Some((manifest, recorded)))
}

/// What `rwv member remove` does beyond dropping the manifest entry.
///
/// `--delete-shared-clone` waives a guard on deleting the canonical clone, so
/// it means nothing without `--delete`; the type has no value for that pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// Drop the entry and leave the checkout on disk.
    Unregister,
    /// Also delete this workspace's checkout.
    DeleteCheckout,
    /// Also delete this workspace's checkout, even when another project lists
    /// the canonical clone.
    DeleteCheckoutAndSharedClone,
}

impl Removal {
    /// The removal the `--delete` and `--delete-shared-clone` flags ask for.
    pub fn from_flags(delete: bool, delete_shared_clone: bool) -> anyhow::Result<Self> {
        match (delete, delete_shared_clone) {
            (false, false) => Ok(Self::Unregister),
            (true, false) => Ok(Self::DeleteCheckout),
            (true, true) => Ok(Self::DeleteCheckoutAndSharedClone),
            (false, true) => Err(crate::untokened!(
                InternalInvariant,
                "`--delete-shared-clone` requires `--delete`"
            )),
        }
    }

    fn deletes_checkout(self) -> bool {
        !matches!(self, Self::Unregister)
    }

    fn waives_shared_clone_guard(self) -> bool {
        matches!(self, Self::DeleteCheckoutAndSharedClone)
    }
}

/// Execute `rwv member remove PATH [--delete] [--delete-shared-clone]`.
///
/// An entry already gone from the manifest is finished the same way, as long
/// as its checkout is still on disk in this workspace and is this project's:
/// the lock still names it, or no other project lists it.
///
/// `--delete` removes the checkout in the workspace `ctx` resolves to: the
/// canonical clone at primary, this workweave's worktree in a workweave. Every
/// guard runs before the manifest is written, so a refused delete leaves it as
/// it was.
///
/// `ctx` is the already-resolved invocation context. Handlers must not
/// re-resolve.
pub fn run_remove(
    path: &str,
    removal: Removal,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let (project, project_dir) = find_project(ctx)?;
    crate::diagnosis::settle_project_drift(&project, &project_dir)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);

    let repo_path = RepoPath::new(path)?;

    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    let before = manifest.clone();
    let removed = manifest.remove_repo(&repo_path);
    let vcs_type = match &removed {
        Some(entry) => entry.vcs_type,
        None => leftover_checkout_vcs(ctx, &project, &project_dir, &repo_path)?,
    };
    let vcs = vcs_for(vcs_type);

    let plan = if removal.deletes_checkout() {
        if let Checkout::Primary { .. } = ctx.checkout {
            refuse_shared_clone_delete(
                ctx,
                &project,
                &repo_path,
                removal.waives_shared_clone_guard(),
            )?;
        }
        Some(crate::sync::plan_checkout_removal(
            vcs.as_ref(),
            ctx,
            &repo_path,
            crate::sync::RemovalCause::RemoveDelete,
        )?)
    } else {
        None
    };

    let departing: &[RepoPath] = match &plan {
        Some(plan) if plan.removes_anything() => std::slice::from_ref(&repo_path),
        _ => &[],
    };
    crate::manifest_commit::judge(&project_dir)?;
    let settled = settle_manifest(
        ctx,
        &project,
        removed.is_some().then_some(&manifest),
        departing,
        opts,
    );
    let on_disk = Manifest::from_path(&manifest_path).ok();
    if on_disk.is_some_and(|m| !m.contains_repo(&repo_path)) {
        commit_removal(ctx, &project, &project_dir, &repo_path, &before)?;
    }
    settled?;

    if removed.is_some() {
        eprintln!("Removed '{}' from manifest", repo_path.as_str());
    } else {
        eprintln!(
            "'{}' was already out of the manifest; regenerated without it",
            repo_path.as_str()
        );
    }

    if let Some(plan) = plan {
        let checkout = ctx.active_path().join(repo_path.as_path());
        let deleted = plan.removes_anything();
        plan.perform(vcs.as_ref(), &repo_path)?;
        if deleted {
            eprintln!("Deleted '{}'", checkout.display());
        }
    }

    Ok(())
}

/// Commit the removal of `repo_path`: its entry, its pin, and what the
/// regeneration without it rewrote.
fn commit_removal(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    project_dir: &Path,
    repo_path: &RepoPath,
    before: &Manifest,
) -> anyhow::Result<()> {
    let on_disk = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let authored = crate::manifest_commit::authored_across(ctx, project, &[before, &on_disk]);
    let lock_path = LockFile::path_in(project_dir);
    if let Some(mut lock) = crate::lock::read_old_lock(&lock_path, "member remove") {
        if lock.repositories.remove(repo_path).is_some() {
            crate::lock::write_lock_in(&lock, project_dir)?;
        }
    }
    let member = repo_path.clone();
    let edit_manifest = move |manifest: &mut Manifest| {
        manifest.remove_repo(&member);
    };
    let member = repo_path.clone();
    let edit_lock = move |lock: &mut LockFile| {
        lock.repositories.remove(&member);
    };
    crate::manifest_commit::commit(&crate::manifest_commit::MemberCommit {
        project,
        project_dir,
        edit_manifest: &edit_manifest,
        edit_lock: Some(&edit_lock),
        authored: &authored,
        message: &format!("member remove {repo_path}\n\n  - {repo_path}: removed"),
        naming: crate::repin::Naming::Unnamed,
    })?;
    Ok(())
}

/// The backend of the checkout `repo_path` names in this workspace when the
/// manifest no longer lists it, refusing unless that checkout is this
/// project's to finish removing: this project's lock still names it, or no
/// other project lists it.
fn leftover_checkout_vcs(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    project_dir: &Path,
    repo_path: &RepoPath,
) -> anyhow::Result<VcsType> {
    let not_in_manifest = || {
        refusal(
            RefusalKind::RepoNotInManifest,
            format!("path '{}' not found in manifest", repo_path.as_str()),
        )
    };
    let lock_path = LockFile::path_in(project_dir);
    let locked = if lock_path.exists() {
        LockFile::from_state(&lock_path)
            .with_context(|| format!("failed to load lock at {lock_path}"))?
            .get_entry(repo_path)
            .map(|entry| entry.vcs_type)
    } else {
        None
    };
    let vcs_type = match locked {
        Some(vcs_type) => vcs_type,
        None if find_other_projects_referencing(ctx.primary_path(), project, repo_path)
            .is_empty() =>
        {
            VcsType::Git
        }
        None => return Err(not_in_manifest()),
    };
    let checkout = ctx.active_path().join(repo_path.as_path());
    if !vcs_for(vcs_type).is_repo(&checkout) {
        return Err(not_in_manifest());
    }
    Ok(vcs_type)
}

/// Refuse to delete the canonical clone of `repo_path` while another project
/// lists it, unless `delete_shared_clone` waives that.
fn refuse_shared_clone_delete(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    repo_path: &RepoPath,
    delete_shared_clone: bool,
) -> anyhow::Result<()> {
    if !ctx.primary_path().join(repo_path.as_path()).exists() {
        return Ok(());
    }
    let referencing_projects =
        find_other_projects_referencing(ctx.primary_path(), project, repo_path);
    for proj in &referencing_projects {
        eprintln!("warning: repo also referenced by project '{proj}'");
    }
    if !referencing_projects.is_empty() && !delete_shared_clone {
        crate::refuse!(
            RefusalKind::SharedCloneReferenced,
            "refusing to delete '{}': referenced by other projects. Remove the entry from those projects first, or use `--delete-shared-clone` if you intend to delete the shared clone anyway.",
            repo_path.as_str()
        );
    }
    Ok(())
}

/// Parse an `rwv member add --new` creation address into the registry it names and
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
                format!("--params-json is not valid JSON: {e:#}"),
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

/// An `rwv member add --new` creation as argv states it: the registry its address
/// names, and the parameter map the address, `--param` and `--params-json`
/// assemble. Its path parameters stay argv text until
/// [`Self::locate_path_params`] reads them from the directory rwv runs in, so
/// the planner sees absolute strings.
pub struct CreationRequest {
    address: String,
    registry: Box<dyn crate::registry::Registry>,
    params: ParamMap,
}

impl CreationRequest {
    pub fn parse(
        address: &str,
        params: &[String],
        params_json: Option<&str>,
    ) -> anyhow::Result<Self> {
        let (registry_name, prefix) = parse_creation_address(address)?;
        let registry = builtin_registries()
            .into_iter()
            .find(|r| r.name() == &registry_name)
            .ok_or_else(|| {
                let names = crate::registry::builtin_registry_names();
                let known: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
                refusal(
                    RefusalKind::UnknownRegistry,
                    format!(
                        "'{}' is not a registry rwv can create through. Known registries: {}. \
                         To add a repo a registry already hosts, use `rwv member add <url>` instead.",
                        registry_name.as_str(),
                        known.join(", ")
                    ),
                )
            })?;
        let params = build_creation_params(prefix, params, params_json)?;
        Ok(Self {
            address: address.to_owned(),
            registry,
            params,
        })
    }

    /// Replace each non-empty [`ParamKind::Path`] value with the location
    /// `locate` reads it as. An empty value names no path and is left for the
    /// planner to refuse.
    pub fn locate_path_params(&mut self, locate: impl Fn(&str) -> PathBuf) -> anyhow::Result<()> {
        for spec in self
            .registry
            .creation_params()
            .iter()
            .filter(|s| s.kind == ParamKind::Path)
        {
            let Some(raw) = self.params.get(spec.name) else {
                continue;
            };
            if raw.is_empty() {
                continue;
            }
            let located = locate(raw);
            let Some(spelled) = located.to_str().map(str::to_owned) else {
                crate::refuse!(
                    RefusalKind::UnusableCreationParam,
                    "creation parameter '{}' resolves to '{}', which is not valid UTF-8",
                    spec.name,
                    located.display()
                );
            };
            self.params.insert(spec.name, spelled);
        }
        Ok(())
    }
}

/// Execute `rwv member add <creation-address> --new [--param NAME=VALUE]...
/// [--params-json JSON] [--role ROLE]`, for a `request` whose path parameters
/// are already located.
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
    request: CreationRequest,
    settings: NewMemberSettings,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let (role, recurse_submodules) = (settings.role(), settings.recurse());
    // `rwv member add` mints the manifest entry, so the backend is an input to the
    // verb rather than a lookup: one value feeds both the handle this verb
    // operates through and the `vcs_type` it records.
    let vcs_type = VcsType::Git;
    let vcs = vcs_for(vcs_type);
    let (project, project_dir) = find_project(ctx)?;
    crate::diagnosis::settle_project_drift(&project, &project_dir)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let CreationRequest {
        address,
        registry,
        params: param_map,
    } = request;
    let registry_name = registry.name().clone();

    let plan = match registry.plan_creation(&param_map) {
        Ok(plan) => plan,
        Err(CreationParamError::Missing(missing)) => {
            let message = missing_creation_param_message(registry.as_ref(), &missing, &param_map);
            crate::refuse!(RefusalKind::MissingCreationParam, "{}", message);
        }
        Err(CreationParamError::NotAbsolute(name)) => {
            crate::refuse!(
                RefusalKind::UnusableCreationParam,
                "creation parameter '{}' must name a path, absolute or relative to the \
                 directory rwv runs in; got '{}'",
                name,
                param_map.get(name).unwrap_or_default()
            );
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

    let url = RepoUrl::from(&plan.url);
    let repo_path =
        crate::registry::placement(&url).expect("a CreationPlan's minted URL is always placeable");

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    // Creation always lives at primary's canonical path (clones are global
    // infrastructure).
    let dest = ctx.primary_path().join(repo_path.as_path());

    if let Some(existing) = manifest.get_entry(&repo_path) {
        if resolve_to_clone_info(&existing.url)?
            .url
            .names_same_remote(&plan.url)
        {
            announce_already_registered(&repo_path);
            place_member(
                vcs.as_ref(),
                ctx,
                &dest,
                &repo_path,
                existing.role,
                existing.recurse_submodules,
            )?;
            return settle_registered(
                ctx,
                &project,
                &project_dir,
                &repo_path,
                &dest,
                vcs.as_ref(),
                opts,
            );
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

    judge_commit_of_add(&project_dir)?;

    // Warn when this repo path is already registered by another project with
    // a potentially different role. The physical init'd repo is shared; the
    // operator should know. This is not a refusal.
    warn_if_shared_clone(ctx.primary_path(), &project, &repo_path, &dest, role);

    let mut left = AddLeftovers::default();
    let mut cloned_from: Option<&Path> = None;
    let added = (|| -> anyhow::Result<()> {
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
                let root_canon = CanonicalPath::existing(root)
                    .with_context(|| format!("failed to resolve root {}", root.display()))?;
                if ctx.primary_root().contains(&root_canon) {
                    crate::refuse!(
                        RefusalKind::UnusableCreationParam,
                        "the root '{}' is inside the weave at '{}'; the upstream it would \
                     create would then be walked, deletable, and reportable as a member \
                     of the very weave it backs — choose a root outside the weave",
                        root.display(),
                        ctx.primary_path().display()
                    );
                }

                if !bare_path.is_dir() {
                    if let Some(parent) = bare_path.parent() {
                        left.make_dir_all(parent)?;
                    }
                    left.made.push(Made::Repo(bare_path.clone()));
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
                        left.make_dir_all(parent)?;
                    }
                    left.made.push(Made::Repo(dest.clone()));
                    vcs.clone_repo(&plan.url, &dest).with_context(|| {
                        format!("failed to clone '{}' into {}", plan.url, dest.display())
                    })?;
                    cloned_from = Some(bare_path);
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
                        left.make_dir_all(parent)?;
                    }
                    left.made.push(Made::Repo(dest.clone()));
                    vcs.init_repo(&dest)
                        .with_context(|| format!("failed to init repo at {}", dest.display()))?;
                    crate::url_remote::ensure(
                        vcs.as_ref(),
                        dest.display(),
                        "member add",
                        &dest,
                        &plan.url,
                    )
                    .with_context(|| format!("failed to add the remote in {}", dest.display()))?;
                }
            }
        }

        let created_on = match vcs.head_attachment(&dest)? {
            HeadAttachment::Unborn(u) => TrackingRef::parse(u.name().clone()),
            HeadAttachment::Attached(a) => TrackingRef::try_from(&a),
            HeadAttachment::Detached(_) => match cloned_from {
                Some(upstream) => {
                    return Err(crate::untokened!(
                    InternalInvariant,
                    "rwv member add: the clone at {} has a detached HEAD right after creation, \
                     taken from the upstream at {upstream}; point that upstream's HEAD at a \
                     branch (`git -C {upstream} symbolic-ref HEAD refs/heads/<branch>`) \
                     before retrying `rwv member add {address} --new`",
                    dest.display(),
                    upstream = upstream.display(),
                ))
                }
                None => {
                    return Err(crate::untokened!(
                        InternalInvariant,
                        "rwv member add: repo at {} has a detached HEAD right after creation; \
                     inspect with `git -C {} status` before retrying `rwv member add {} --new`",
                        dest.display(),
                        dest.display(),
                        address
                    ))
                }
            },
        };

        let branch = declared_branch(
            created_on,
            vcs.as_ref(),
            &repo_path,
            &format!("the branch the repo at {} was created on", dest.display()),
            || {
                format!(
                    "rwv member add: '{}' at {}: the branch the repo was created on cannot be its \
                     declared `branch`",
                    repo_path.as_str(),
                    dest.display(),
                )
            },
        )?;

        let entry = RepoEntry {
            vcs_type,
            url,
            branch,
            role,
            recurse_submodules,
            history: None,
        };
        manifest.insert_repo(repo_path.clone(), entry);

        // In a workweave, attempt to create a worktree at the workweave so the
        // new repo is materialized there. An unborn HEAD (both creation shapes
        // start unborn) means create_worktree_in_workweave skips until the first
        // commit lands.
        place_member_noting(
            vcs.as_ref(),
            ctx,
            &dest,
            &repo_path,
            role,
            recurse_submodules,
            &mut left,
        )?;

        settle_and_commit(
            ctx,
            &project,
            &project_dir,
            &manifest,
            &repo_path,
            &dest,
            vcs.as_ref(),
            opts,
        )
    })();
    added.map_err(|e| {
        left.refused(
            e,
            &manifest_path,
            &repo_path,
            &format!("rwv member add {address} --new"),
        )
    })?;

    eprintln!("Added new repo '{}' to manifest", repo_path.as_str());
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
/// Used by `rwv member add` to emit shared-clone warnings and by `rwv member remove
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
        let manifest_path = project_dir(workspace_root, &project).join(Manifest::FILE_NAME);
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
            other_role,
            this_role,
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
        kind: crate::registry::ParamKind::Text,
        help: "the warehouse the widgets ship from",
    }];

    impl crate::registry::Registry for FixtureRegistry {
        fn name(&self) -> &RegistryName {
            &self.registry_name
        }
        fn matches(&self, _raw: &str) -> Option<RepoUrl> {
            None
        }
        fn clone_url(&self, _id: &RepoId) -> Option<crate::registry::CloneUrl> {
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

    #[test]
    fn every_removal_comes_from_the_flags_that_ask_for_it_and_no_other_pair_parses() {
        let all = [
            Removal::Unregister,
            Removal::DeleteCheckout,
            Removal::DeleteCheckoutAndSharedClone,
        ];
        for removal in all {
            let flags = (
                removal.deletes_checkout(),
                removal.waives_shared_clone_guard(),
            );
            assert_eq!(
                Removal::from_flags(flags.0, flags.1).unwrap(),
                removal,
                "{removal:?} read back from its own flags"
            );
        }

        let parsed = [(false, false), (false, true), (true, false), (true, true)]
            .into_iter()
            .filter(|&(delete, shared)| Removal::from_flags(delete, shared).is_ok())
            .count();
        assert_eq!(
            parsed,
            all.len(),
            "a flag pair parses with no Removal of its own"
        );
        assert!(Removal::from_flags(false, true).is_err());
    }
}
