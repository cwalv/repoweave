//! The permissions a member's tracked regular files hold for its role: no
//! write bit on a `reference` member's, the owner's write bit on every other
//! member's.
//!
//! Only tracked regular files are touched. chmod follows a symlink, so a
//! tracked symlink is never chmod'ed: that would change its target, possibly
//! outside the repository. Directories keep their write bits, which is what
//! lets the VCS replace a read-only file during an update, and what lets an
//! untracked file still be created in a `reference` member.

use crate::manifest::{Manifest, ProjectName, RepoPath, Role};
use crate::share::FilePlacement;
use crate::vcs::{vcs_for, ResolvedRevisionId, TrackedFileKind, Vcs};
use anyhow::Context;
use std::fs::Metadata;
use std::path::Path;

/// Whose manifests decide the role of a checkout, and what each says.
pub(crate) enum Listing {
    /// A workweave's checkout serves its own project alone.
    Workweave(Option<Role>),
    /// A primary's checkout is the one clone every project listing it shares,
    /// so it holds `reference` only where all of them declare it.
    Primary(Vec<(ProjectName, Role)>),
    /// Not a member slot of any weave.
    Unplaced,
}

impl Listing {
    /// The checkout at `checkout`, by the manifests on disk where it lives.
    pub(crate) fn of(checkout: &Path) -> Listing {
        for root in checkout.ancestors().skip(1) {
            let Ok(repo) = RepoPath::below(root, checkout) else {
                return Listing::Unplaced;
            };
            let role_in = |project: &crate::manifest::ProjectName| {
                Manifest::from_path(
                    &crate::workspace::project_dir(root, project).join(Manifest::FILE_NAME),
                )
                .ok()
                .and_then(|m| m.get_entry(&repo).map(|e| e.role))
            };
            if let Some(marker) = crate::workspace::WorkweaveMarker::read(root).ok().flatten() {
                return Listing::Workweave(role_in(marker.project()));
            }
            if crate::workspace::projects_dir(root).is_dir() {
                return Listing::Primary(
                    crate::workspace::discover_projects(root)
                        .into_iter()
                        .filter_map(|p| role_in(&p).map(|role| (p, role)))
                        .collect(),
                );
            }
        }
        Listing::Unplaced
    }

    /// Whether the checkout's files hold `reference`: some manifest lists it,
    /// and every one that does declares `reference`.
    pub(crate) fn is_reference(&self) -> bool {
        match self {
            Listing::Workweave(role) => *role == Some(Role::Reference),
            Listing::Primary(roles) => {
                !roles.is_empty() && roles.iter().all(|(_, role)| *role == Role::Reference)
            }
            Listing::Unplaced => false,
        }
    }

    /// The projects listing a primary's checkout, with their roles, when some
    /// declare `reference` and some do not.
    pub(crate) fn disagreement(&self) -> Option<&[(ProjectName, Role)]> {
        match self {
            Listing::Primary(roles)
                if roles.iter().any(|(_, r)| *r == Role::Reference)
                    && roles.iter().any(|(_, r)| *r != Role::Reference) =>
            {
                Some(roles)
            }
            _ => None,
        }
    }
}

/// The role the files of the checkout at `member` hold, for a member its
/// caller's project declares `declared`: in a primary, the first role other
/// than `reference` any project listing the checkout declares, if one does.
pub fn role_held(member: &Path, declared: Role) -> Role {
    match Listing::of(member) {
        Listing::Primary(roles) if declared == Role::Reference => roles
            .into_iter()
            .map(|(_, role)| role)
            .find(|role| *role != Role::Reference)
            .unwrap_or(Role::Reference),
        _ => declared,
    }
}

/// Bring the tracked regular files of the member checked out at `member` to
/// the permissions [`role_held`] gives it for a member its caller's project
/// declares `role`, and return how many files changed.
///
/// A file already in that state is left alone, so its ctime does not move. A
/// read-only file of a non-`reference` member that shares its inode with
/// another file is replaced by a private copy before it is made writable:
/// write bits belong to the inode, so restoring them in place would make
/// every workspace sharing it writable.
pub fn settle(vcs: &dyn Vcs, member: &Path, role: Role) -> anyhow::Result<usize> {
    settle_held(vcs, member, role_held(member, role))
}

fn settle_held(vcs: &dyn Vcs, member: &Path, role: Role) -> anyhow::Result<usize> {
    let mut changed = 0;
    for file in tracked_regular_files(vcs, member)? {
        let path = member.join(&file);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() || holds_role(&meta, role) {
            continue;
        }
        if role == Role::Reference {
            set_writable(&path, &meta, false)
        } else if shares_inode(&meta) {
            unshare(&path)
        } else {
            set_writable(&path, &meta, true)
        }
        .with_context(|| format!("failed to settle {}", path.display()))?;
        changed += 1;
    }
    Ok(changed)
}

/// What settling a member did.
#[derive(Debug)]
pub struct Settled {
    /// Tracked files whose permissions changed.
    pub permissions: usize,
    /// How the files a move rewrote were placed, where the backend could say.
    pub files: Option<FilePlacement>,
}

/// [`settle`] the member at `member` after the backend moved it from `from`
/// to its current revision: each file the move rewrote is first cloned again
/// from the repository's other workspaces where it can be, since the backend
/// writes every replaced file as a new, unshared one.
pub fn settle_moved(
    vcs: &dyn Vcs,
    member: &Path,
    role: Role,
    from: Option<&ResolvedRevisionId>,
) -> anyhow::Result<Settled> {
    let role = role_held(member, role);
    let files = match from {
        Some(from) => {
            let to = vcs.head_revision(member)?;
            if *from == to {
                None
            } else {
                let changed = vcs.changed_paths_between(member, from, &to)?;
                crate::share::reshare(vcs, member, role, &changed, &crate::share::Filesystem)
                    .with_context(|| format!("failed to share files in {}", member.display()))?
            }
        }
        None => None,
    };
    Ok(Settled {
        permissions: settle_held(vcs, member, role)?,
        files,
    })
}

/// How many tracked regular files of the member at `member` do not hold the
/// permissions `role` asks for. `role` is the one the files hold, not one a
/// project declares: [`role_held`] answers that.
pub fn unsettled(vcs: &dyn Vcs, member: &Path, role: Role) -> anyhow::Result<usize> {
    Ok(tracked_regular_files(vcs, member)?
        .into_iter()
        .filter(|file| {
            std::fs::symlink_metadata(member.join(file))
                .is_ok_and(|meta| meta.file_type().is_file() && !holds_role(&meta, role))
        })
        .count())
}

/// [`settle`] every member of `manifest` checked out under `workspace`, each
/// for the role `manifest` gives it. A member slot that is absent or a
/// symlink is not a checkout of the member and is skipped.
///
/// Returns the members that failed, with why.
pub fn settle_workspace(workspace: &Path, manifest: &Manifest) -> Vec<(RepoPath, anyhow::Error)> {
    settle_workspace_moved(workspace, manifest, &|_, _| None)
        .into_iter()
        .filter_map(|(repo, settled)| settled.err().map(|e| (repo, e)))
        .collect()
}

/// [`settle_moved`] every member of `manifest` checked out under
/// `workspace`, each from the revision `moved_from` answers for its checkout.
pub fn settle_workspace_moved(
    workspace: &Path,
    manifest: &Manifest,
    moved_from: &dyn Fn(&dyn Vcs, &Path) -> Option<ResolvedRevisionId>,
) -> Vec<(RepoPath, anyhow::Result<Settled>)> {
    manifest
        .iter_entries()
        .filter_map(|(repo, entry)| {
            let member = workspace.join(repo.as_path());
            if member.is_symlink() || !member.is_dir() {
                return None;
            }
            let vcs = vcs_for(entry.vcs_type);
            let from = moved_from(vcs.as_ref(), &member);
            Some((
                repo.clone(),
                settle_moved(vcs.as_ref(), &member, entry.role, from.as_ref()),
            ))
        })
        .collect()
}

/// A backend that cannot list its tracked files has nothing settle can name.
fn tracked_regular_files(vcs: &dyn Vcs, member: &Path) -> anyhow::Result<Vec<String>> {
    let Some(population) = vcs.population() else {
        return Ok(Vec::new());
    };
    Ok(population
        .tracked_files(member)?
        .into_iter()
        .filter(|f| matches!(f.kind, TrackedFileKind::File { .. }))
        .map(|f| f.path)
        .collect())
}

fn holds_role(meta: &Metadata, role: Role) -> bool {
    if role == Role::Reference {
        !any_write_bit(meta)
    } else {
        owner_can_write(meta)
    }
}

/// Replace the file at `path` by a writable private copy, leaving every other
/// name for its inode holding the old one.
fn unshare(path: &Path) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let private = path.with_file_name(format!(".{name}.rwv-unshare"));
    let replaced = std::fs::copy(path, &private)
        .and_then(|_| std::fs::symlink_metadata(&private))
        .and_then(|meta| set_writable(&private, &meta, true))
        .and_then(|()| std::fs::rename(&private, path));
    if replaced.is_err() {
        let _ = std::fs::remove_file(&private);
    }
    replaced
}

#[cfg(unix)]
pub(crate) fn any_write_bit(meta: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o222 != 0
}

#[cfg(unix)]
fn owner_can_write(meta: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o200 != 0
}

#[cfg(unix)]
fn shares_inode(meta: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.nlink() > 1
}

#[cfg(unix)]
fn set_writable(path: &Path, meta: &Metadata, writable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    let mode = if writable {
        mode | 0o200
    } else {
        mode & !0o222
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
pub(crate) fn any_write_bit(meta: &Metadata) -> bool {
    !meta.permissions().readonly()
}

#[cfg(windows)]
fn owner_can_write(meta: &Metadata) -> bool {
    !meta.permissions().readonly()
}

/// Stable Rust exposes no link count on Windows, and no backend declares the
/// hardlink mode there, so nothing rwv makes shares an inode on Windows.
#[cfg(windows)]
fn shares_inode(_meta: &Metadata) -> bool {
    false
}

#[cfg(windows)]
fn set_writable(path: &Path, meta: &Metadata, writable: bool) -> std::io::Result<()> {
    let mut permissions = meta.permissions();
    permissions.set_readonly(!writable);
    std::fs::set_permissions(path, permissions)
}
