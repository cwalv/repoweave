//! The commit a member verb makes for its own change: the entry it edited,
//! the pin it moved, and the files its regeneration rewrote, with every other
//! uncommitted edit to the project repo left where it stands.

use crate::manifest::{LockFile, Manifest, Project, ProjectName};
use crate::vcs::{project_vcs, Vcs};
use anyhow::Context;
use std::collections::BTreeSet;
use std::path::Path;

/// One verb's change to the project repo.
pub(crate) struct MemberCommit<'a> {
    pub(crate) project: &'a ProjectName,
    pub(crate) project_dir: &'a Path,
    pub(crate) edit_manifest: &'a dyn Fn(&mut Manifest),
    pub(crate) edit_lock: Option<&'a dyn Fn(&mut LockFile)>,
    pub(crate) authored: &'a BTreeSet<String>,
    pub(crate) message: &'a str,
    pub(crate) naming: crate::repin::Naming,
}

/// The files a regeneration across `manifests` may rewrite: the owned paths
/// of each, since a regeneration rewrites what the new manifest declares and
/// strips what only the old one did.
pub(crate) fn authored_across(
    ctx: &crate::workspace::WorkspaceContext,
    project: &ProjectName,
    manifests: &[&Manifest],
) -> BTreeSet<String> {
    let dir = crate::workspace::project_dir(ctx.active_path(), project);
    let committed = committed_manifest(project_vcs().as_ref(), &dir)
        .ok()
        .flatten();
    manifests
        .iter()
        .copied()
        .chain(committed.as_ref())
        .flat_map(|manifest| crate::activate::owned_paths(ctx.active_path(), project, manifest))
        .collect()
}

/// What a commit would leave out, judged before the verb writes anything.
pub(crate) fn judge(project_dir: &Path) -> anyhow::Result<()> {
    let vcs = project_vcs();
    if !vcs.is_repo(project_dir) {
        return Err(crate::untokened!(
            Owed,
            "{} is not a repository, so there is nowhere to commit the change",
            project_dir.display()
        ));
    }
    crate::op_state::check_no_op_in_progress(&[project_dir])?;
    Ok(())
}

pub(crate) fn committed_manifest(
    vcs: &dyn Vcs,
    project_dir: &Path,
) -> anyhow::Result<Option<Manifest>> {
    let Some(text) = committed_text(vcs, project_dir) else {
        return Ok(None);
    };
    Manifest::from_toml_str(&text)
        .map(Some)
        .with_context(|| format!("the committed {} does not parse", Manifest::FILE_NAME))
}

fn committed_text(vcs: &dyn Vcs, project_dir: &Path) -> Option<String> {
    let head = vcs.head_revision(project_dir).ok()?;
    vcs.read_file_at_revision(project_dir, &head, Path::new(Manifest::FILE_NAME))
        .ok()
}

fn committed_lock(vcs: &dyn Vcs, project_dir: &Path) -> Option<LockFile> {
    let head = vcs.head_revision(project_dir).ok()?;
    let text = vcs
        .read_file_at_revision(project_dir, &head, Path::new(LockFile::FILE_NAME))
        .ok()?;
    LockFile::from_json_str(&text).ok()
}

fn lock_bytes(lock: &LockFile) -> anyhow::Result<Vec<u8>> {
    let mut json = serde_json::to_string_pretty(lock).context("failed to serialize lock file")?;
    json.push('\n');
    Ok(json.into_bytes())
}

/// Commit the change: HEAD's `rwv.toml` and `rwv.lock` with the edits applied,
/// staged as content so that nothing else the working files carry is in the
/// commit, and the regenerated files besides when the working manifest holds
/// no other edit. Returns whether a commit was made.
pub(crate) fn commit(change: &MemberCommit<'_>) -> anyhow::Result<bool> {
    let vcs = project_vcs();
    let vcs = vcs.as_ref();
    let dir = change.project_dir;
    let manifest_file = Path::new(Manifest::FILE_NAME);
    let working = Project::from_dir_skip_lock(dir, change.project.clone())
        .with_context(|| format!("failed to load project '{}'", change.project))?
        .manifest;

    let base_text = match committed_text(vcs, dir) {
        Some(text) => text,
        None => std::fs::read_to_string(dir.join(manifest_file))
            .with_context(|| format!("failed to read {}", dir.join(manifest_file).display()))?,
    };
    let (edited_text, edited) = Manifest::edit_text(&base_text, |m| (change.edit_manifest)(m))
        .with_context(|| format!("the committed {} does not parse", Manifest::FILE_NAME))?;
    let carries_only_this_change = edited.declares_same(&working);

    let mut paths: Vec<String> = Vec::new();
    vcs.stage_content(dir, manifest_file, edited_text.as_bytes())
        .context("failed to record rwv.toml for the commit")?;
    paths.push(Manifest::FILE_NAME.to_owned());

    let mut staged_lock = None;
    if let Some(edit_lock) = change.edit_lock {
        let mut lock = committed_lock(vcs, dir).unwrap_or(LockFile {
            repositories: Default::default(),
        });
        edit_lock(&mut lock);
        vcs.stage_content(dir, Path::new(LockFile::FILE_NAME), &lock_bytes(&lock)?)
            .context("failed to record rwv.lock for the commit")?;
        paths.push(LockFile::FILE_NAME.to_owned());
        staged_lock = Some(lock);
    }

    if carries_only_this_change {
        let dirty = vcs
            .dirty_file_names(dir)
            .context("failed to read project repo status")?;
        let carried: Vec<String> = dirty
            .into_iter()
            .filter(|path| change.authored.contains(path.as_str()))
            .collect();
        if !carried.is_empty() {
            let refs: Vec<&str> = carried.iter().map(String::as_str).collect();
            vcs.stage_paths(dir, &refs)
                .context("failed to record the regenerated files for the commit")?;
            paths.extend(carried);
        }
    }

    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let committed = match crate::repin::commit(
        vcs,
        dir,
        &refs,
        change.message,
        staged_lock.as_ref(),
        &change.naming,
    ) {
        Ok(committed) => committed,
        Err(refused) => {
            let refused = refused.context("failed to commit the change");
            return Err(match vcs.unstage_paths(dir, &refs) {
                Ok(()) => refused,
                Err(e) => refused.context(format!(
                    "{} left marked for commit: taking them back out after the refused commit failed ({e:#})",
                    paths.join(", ")
                )),
            });
        }
    };
    if committed {
        eprintln!("Committed {}", paths.join(", "));
    }
    crate::lock::report_left_edits(vcs, change.project, dir, change.authored)?;
    Ok(committed)
}
