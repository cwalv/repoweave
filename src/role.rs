//! `rwv role` — change a member's role in this workspace's manifest, and
//! settle its files for the new role.

use crate::add_remove::{find_project, settle_manifest};
use crate::manifest::{Manifest, RepoPath, Role};
use crate::refusal::RefusalKind;
use crate::vcs::vcs_for;
use crate::workspace::WorkspaceContext;
use anyhow::Context;

/// Execute `rwv role <repo> <role>`.
///
/// The manifest edit and the regeneration it drives land together or not at
/// all, since a role can move a member into or out of an ecosystem's build
/// graph: `reference` members are excluded from it. The member is then
/// settled for whichever role `rwv.toml` holds afterwards, so a regeneration
/// that put the edit back leaves the files as they were.
///
/// A role `rwv.toml` already holds is not rewritten, but the member's files are
/// still settled for it, so a hand edit of the role is finished by re-running
/// this.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_role(repo: &str, role: Role, ctx: &WorkspaceContext) -> anyhow::Result<()> {
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
    let vcs = vcs_for(entry.vcs_type);
    let member = ctx.active_path().join(repo_path.as_path());
    let checked_out = !member.is_symlink() && vcs.is_repo(&member);
    if role == Role::Reference && checked_out {
        let dirty = vcs
            .tracked_dirty_file_names(&member)
            .with_context(|| format!("failed to read the status of '{repo_path}'"))?;
        if !dirty.is_empty() {
            crate::refuse!(
                RefusalKind::DirtyCheckout,
                "refusing to make '{repo_path}' a reference member: it has uncommitted \
                 changes to tracked files, which would become read-only:\n  {}\n\n\
                 Commit them, stash them, or discard them, then re-run.",
                dirty.join("\n  ")
            );
        }
    }

    if was == role {
        let changed = if checked_out {
            crate::settle::settle(vcs.as_ref(), &member, role)
                .with_context(|| format!("failed to settle the files of '{repo_path}'"))?
        } else {
            0
        };
        if changed == 0 {
            eprintln!("'{repo_path}' is already {role}; nothing to change");
        } else {
            eprintln!("'{repo_path}' is already {role}; settled {changed} file(s)");
        }
        return Ok(());
    }

    entry.role = role;
    manifest.insert_repo(repo_path.clone(), entry);
    let landed = settle_manifest(ctx, &project, Some(&manifest));

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
    landed?;
    settled?;
    eprintln!("Changed the role of '{repo_path}' from {was} to {role}");
    Ok(())
}
