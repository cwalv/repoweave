//! `rwv member set-role` and `rwv member set-recurse-submodules` — change one
//! field of a member's entry in this workspace's manifest, and settle what the
//! change reaches.

use crate::activate::ActivateOptions;
use crate::add_remove::{find_project, settle_manifest};
use crate::manifest::{Manifest, RepoPath, Role, SubmoduleRecursion};
use crate::refusal::RefusalKind;
use crate::vcs::vcs_for;
use crate::workspace::WorkspaceContext;
use anyhow::Context;
use std::collections::BTreeSet;

/// Commit `apply` to `repo_path`'s entry as HEAD has it, with what the
/// verb's regeneration rewrote and nothing else the project repo holds
/// uncommitted.
fn commit_entry(
    ctx: &WorkspaceContext,
    project: &crate::manifest::ProjectName,
    project_dir: &std::path::Path,
    repo_path: &RepoPath,
    before: &Manifest,
    apply: impl Fn(&mut crate::manifest::RepoEntry),
    message: String,
) -> anyhow::Result<()> {
    let on_disk = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let authored: BTreeSet<String> =
        crate::manifest_commit::authored_across(ctx, project, &[before, &on_disk]);
    let member = repo_path.clone();
    let edit = move |manifest: &mut Manifest| {
        if let Some(mut entry) = manifest.get_entry(&member).cloned() {
            apply(&mut entry);
            manifest.insert_repo(member.clone(), entry);
        }
    };
    crate::manifest_commit::commit(&crate::manifest_commit::MemberCommit {
        project,
        project_dir,
        edit_manifest: &edit,
        edit_lock: None,
        authored: &authored,
        message: &message,
        naming: crate::repin::Naming::Unnamed,
    })?;
    Ok(())
}

/// Execute `rwv member set-role <repo> <role>`.
///
/// The manifest edit and the regeneration it drives land together or not at
/// all, since a role can move a member into or out of an ecosystem's build
/// graph: `reference` members are excluded from it. The member is then
/// settled for whichever role `rwv.toml` holds afterwards, so a regeneration
/// that put the edit back leaves the files as they were.
///
/// A role `rwv.toml` already holds is not rewritten, but the generated files are
/// regenerated and the member's files settled for it, so a hand edit of the role is finished by re-running
/// this.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_set_role(
    repo: &str,
    role: Role,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let (project, project_dir) = find_project(ctx)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let repo_path = RepoPath::new(repo)?;
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    let Some(mut entry) = manifest.get_entry(&repo_path).cloned() else {
        crate::refuse!(
            RefusalKind::RepoNotInManifest,
            "path '{}' not found in manifest",
            repo_path.as_str()
        );
    };
    let was = entry.role;
    let before = manifest.clone();
    crate::manifest_commit::judge(&project_dir)?;
    let vcs = vcs_for(entry.vcs_type);
    let member = ctx.active_path().join(repo_path.as_path());
    let checked_out = !member.is_symlink() && vcs.is_repo(&member);
    if role == Role::Reference && checked_out {
        let dirty = vcs
            .tracked_dirty_file_names(&member)
            .with_context(|| format!("failed to read the status of '{repo_path}'"))?;
        if !dirty.is_empty() {
            let mut report = crate::diagnosis::Report::default();
            report.add(
                crate::diagnosis::dirty(vcs.as_ref(), &repo_path, &member, dirty)
                    .checkout(&member)
                    .judge(vcs.as_ref()),
            );
            report.into_result()?;
        }
    }

    if was == role {
        settle_manifest(ctx, &project, None, &[], opts)?;
        let changed = if checked_out {
            crate::settle::settle(vcs.as_ref(), &member, role)
                .with_context(|| format!("failed to settle the files of '{repo_path}'"))?
        } else {
            0
        };
        commit_entry(
            ctx,
            &project,
            &project_dir,
            &repo_path,
            &before,
            |entry| entry.role = role,
            format!("member set-role {repo_path} {role}\n\n  - {repo_path}: role {role}"),
        )?;
        if changed == 0 {
            eprintln!("'{repo_path}' is already {role}; nothing to change");
        } else {
            eprintln!("'{repo_path}' is already {role}; settled {changed} file(s)");
        }
        return Ok(());
    }

    entry.role = role;
    manifest.insert_repo(repo_path.clone(), entry);
    let landed = settle_manifest(ctx, &project, Some(&manifest), &[], opts);

    let on_disk = Manifest::from_path(&manifest_path)
        .ok()
        .and_then(|m| m.get_entry(&repo_path).map(|e| e.role));
    let settled = match on_disk {
        Some(on_disk) if checked_out => crate::settle::settle(vcs.as_ref(), &member, on_disk)
            .map(|_| ())
            .with_context(|| {
                format!(
                    "rwv.toml gives '{repo_path}' the role {on_disk}, but its files could \
                     not be settled for it; `rwv doctor --fix` retries"
                )
            }),
        _ => Ok(()),
    };
    let committed = if landed.is_ok() || on_disk == Some(role) {
        commit_entry(
            ctx,
            &project,
            &project_dir,
            &repo_path,
            &before,
            |entry| entry.role = role,
            format!("member set-role {repo_path} {role}\n\n  - {repo_path}: role {was} -> {role}"),
        )
    } else {
        Ok(())
    };
    landed?;
    settled?;
    committed?;
    eprintln!("Changed the role of '{repo_path}' from {was} to {role}");
    Ok(())
}

/// Execute `rwv member set-recurse-submodules <repo> <true|false>`.
///
/// The setting decides whether a new checkout of the member populates the
/// submodules it records; a checkout that already exists keeps what it has.
/// A value `rwv.toml` already holds is not rewritten, but the generated files
/// are regenerated, so a hand edit of the setting is finished by re-running
/// this.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_set_recurse_submodules(
    repo: &str,
    recursion: SubmoduleRecursion,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let (project, project_dir) = find_project(ctx)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let repo_path = RepoPath::new(repo)?;
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    let Some(mut entry) = manifest.get_entry(&repo_path).cloned() else {
        crate::refuse!(
            RefusalKind::RepoNotInManifest,
            "path '{}' not found in manifest",
            repo_path.as_str()
        );
    };
    let was = entry.recurse_submodules;
    let recurse = bool::from(recursion);
    let before = manifest.clone();
    crate::manifest_commit::judge(&project_dir)?;
    if was == recursion {
        settle_manifest(ctx, &project, None, &[], opts)?;
        commit_entry(
            ctx,
            &project,
            &project_dir,
            &repo_path,
            &before,
            |entry| entry.recurse_submodules = recursion,
            format!(
                "member set-recurse-submodules {repo_path} {recurse}\n\n  - {repo_path}: \
                 recurse-submodules {recurse}"
            ),
        )?;
        eprintln!("'{repo_path}' already has recurse-submodules = {recurse}; nothing to change");
        return Ok(());
    }

    entry.recurse_submodules = recursion;
    manifest.insert_repo(repo_path.clone(), entry);
    let landed = settle_manifest(ctx, &project, Some(&manifest), &[], opts);
    let on_disk = Manifest::from_path(&manifest_path)
        .ok()
        .and_then(|m| m.get_entry(&repo_path).map(|e| e.recurse_submodules));
    if landed.is_ok() || on_disk == Some(recursion) {
        commit_entry(
            ctx,
            &project,
            &project_dir,
            &repo_path,
            &before,
            |entry| entry.recurse_submodules = recursion,
            format!(
                "member set-recurse-submodules {repo_path} {recurse}\n\n  - {repo_path}: \
                 recurse-submodules {} -> {recurse}",
                bool::from(was)
            ),
        )?;
    }
    landed?;
    eprintln!(
        "Changed recurse-submodules of '{repo_path}' from {} to {recurse}",
        bool::from(was)
    );
    Ok(())
}
