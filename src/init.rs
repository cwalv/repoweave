//! Project initialization: `rwv init`.
//!
//! Creates a new project directory under `projects/`, runs `git init`,
//! writes an empty `rwv.toml`, authors the integration files it implies,
//! commits exactly those files as the project's first commit, and
//! auto-activates the project. Optionally configures a git remote when
//! `--provider` is given.
//!
//! ## Empty-directory bootstrap
//!
//! When invoked in an empty directory (no workspace markers), `rwv init`
//! creates the minimal workspace skeleton (`projects/`) before proceeding.
//! This makes the standard day-0 flow work immediately:
//!
//! ```text
//! mkdir my-ws && cd my-ws
//! rwv init my-project        # bootstraps projects/ and initializes the project
//! rwv member add <url>              # works — workspace context resolves
//! ```
//!
//! Running `rwv init` in a non-empty, non-workspace directory is refused with
//! a clear actionable error.

use crate::manifest::Manifest;
use crate::refusal::{refusal, RefusalKind};
use crate::registry::{builtin_registries, CloneUrl, RepoId};
use crate::vcs::project_vcs;
use crate::workspace::{
    create_identity_dir, enclosing_project, project_dir, projects_dir, require_workspace_or_empty,
    warn_confusable_project_siblings, MintedDir, WorkspaceContext,
};
use anyhow::Context;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Bootstrap a workspace skeleton in `cwd` if it is an empty directory.
///
/// Reuses [`require_workspace_or_empty`] as the gate:
///
/// - Already a workspace → no-op; the caller's subsequent [`WorkspaceContext::resolve_invocation`]
///   will succeed as before.
/// - Empty directory → creates `projects/` so that [`WorkspaceContext::resolve_invocation`]
///   can find a workspace marker and proceed.
/// - Non-empty, non-workspace directory → returns a clear refusal error naming
///   the state, what `init` would do, and the next step.
fn bootstrap_workspace_if_empty(cwd: &Path) -> anyhow::Result<()> {
    // Reuse the shared gate — don't duplicate emptiness / workspace logic.
    // Map the error to an init-specific message (the generic message mentions
    // --allow-non-empty-dir, which `init` does not expose).
    require_workspace_or_empty(cwd, false).map_err(|_| {
        crate::refusal::refusal(
            crate::refusal::RefusalKind::OccupiedBootstrapDir,
            format!(
                "`rwv init` requires either an existing workspace or an empty directory; \
                 {} is not a workspace and is not empty. \
                 `rwv init` would create a workspace skeleton (projects/) and initialize a \
                 project. To proceed: run `rwv init` in an empty directory, or `cd` into an \
                 existing workspace.",
                cwd.display()
            ),
        )
    })?;

    // Gate passed. If we are NOT already a workspace (resolve would fail), we
    // are in an empty dir — create the minimal `projects/` marker so that the
    // subsequent `WorkspaceContext::resolve_invocation` call in `init` finds
    // a workspace root.
    if WorkspaceContext::resolve_invocation(cwd, None).is_err() {
        let projects_dir = projects_dir(cwd);
        std::fs::create_dir_all(&projects_dir)
            .with_context(|| format!("failed to create {}", projects_dir.display()))?;
        eprintln!(
            "Bootstrapped workspace at {} (created projects/)",
            cwd.display()
        );
    }

    Ok(())
}

/// Initialize a new project in the workspace.
///
/// - Bootstraps a workspace skeleton if `cwd` is an empty directory.
/// - Resolves the workspace root from `cwd`.
/// - Creates `projects/{name}/`.
/// - Runs `git init` in the new directory.
/// - Writes an empty `rwv.toml` (`repositories: {}`).
/// - If `provider` is given (e.g., `"github/owner"`), configures a git remote.
/// - Authors the integration files the empty manifest implies, commits what
///   it wrote, then activates the project (writes `.rwv-active` and surfaces
///   them).
pub fn init(
    name: &crate::manifest::ProjectName,
    provider: Option<&str>,
    origin_dir: &Path,
) -> anyhow::Result<()> {
    let remote_url = provider
        .map(|p| provider_remote_url(p, name.as_str()))
        .transpose()?;
    let vcs = project_vcs();
    judge_commit_identity(vcs.as_ref(), origin_dir)?;

    // Bootstrap here rather than at dispatch — the origin dir may be empty
    // (no workspace to resolve yet), and `bootstrap_workspace_if_empty`
    // creates the minimal skeleton so the follow-on `resolve` succeeds. This
    // resolve is a first-resolution of the freshly-bootstrapped workspace,
    // not a re-resolution of the invocation context (which dispatch could
    // not compute pre-bootstrap).
    bootstrap_workspace_if_empty(origin_dir)?;
    let ctx = WorkspaceContext::resolve_invocation(origin_dir, None)?;
    let project_dir = project_dir(ctx.primary_path(), name);

    if let Some(enclosing) = enclosing_project(ctx.primary_path(), name) {
        crate::refuse!(
            RefusalKind::NestedProjectName,
            "cannot create project '{name}': `projects/{enclosing}/` is already a project, \
             and rwv reads everything below a project's directory as that project's own \
             files — a project there would exist on disk and never be listed. Choose a name \
             outside `{enclosing}/`, or work in `{enclosing}` itself."
        );
    }

    // The collision check IS the creation: asking the filesystem to make the
    // directory is what consults it about its own equivalence. A prior
    // `exists()` test answers the same question one call earlier and then has
    // to guess at the occupant, which is how git ends up naming the spelling
    // that was asked for rather than the one that is there.
    if let MintedDir::Occupied(occupant) = create_identity_dir(&project_dir)? {
        crate::refuse!(
            RefusalKind::ProjectDirOccupied,
            "cannot create project '{name}': {}. Choose another name, or work \
             in the project that is already there.",
            occupant.describe()
        );
    }
    warn_confusable_project_siblings(ctx.primary_path(), name);

    vcs.init_repo(&project_dir)
        .with_context(|| format!("failed to create a repo in {}", project_dir.display()))?;

    std::fs::write(project_dir.join(Manifest::FILE_NAME), Manifest::SKELETON)
        .with_context(|| format!("failed to write {}", Manifest::FILE_NAME))?;

    let declaration_file = crate::lock_driver::declare_in_working_copy(vcs.as_ref(), &project_dir)?
        .then(|| PathBuf::from(vcs.lock_driver_declaration_file()));
    vcs.define_lock_driver(&project_dir).with_context(|| {
        format!(
            "failed to define the lock merge driver in {}",
            project_dir.display()
        )
    })?;

    if let Some(url) = remote_url {
        let remote = crate::url_remote::ensure(vcs.as_ref(), name, "init", &project_dir, &url)
            .with_context(|| format!("failed to add the remote in {}", project_dir.display()))?;
        vcs.record_project_url_remote(&project_dir, &remote)
            .with_context(|| {
                format!(
                    "failed to record the url remote in {}",
                    project_dir.display()
                )
            })?;
    }

    record_trunk(vcs.as_ref(), name, &project_dir)?;
    crate::alignment::record(ctx.primary_path(), name)?;

    eprintln!(
        "Initialized project '{}' at {}",
        name,
        project_dir.display()
    );

    crate::activate::author_integration_files(ctx.primary_path(), name)?;
    let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let mut written: BTreeSet<PathBuf> =
        crate::activate::owned_paths(ctx.primary_path(), name, &manifest)
            .into_iter()
            .map(PathBuf::from)
            .collect();
    written.insert(PathBuf::from(Manifest::FILE_NAME));
    written.extend(declaration_file);
    commit_written(
        vcs.as_ref(),
        &project_dir,
        &written,
        &format!("init {name}"),
    )?;
    crate::activate::activate(name, &ctx)?;

    Ok(())
}

/// Refuse before anything is written when a commit made from `dir` would have
/// no author to record.
pub(crate) fn judge_commit_identity(vcs: &dyn crate::vcs::Vcs, dir: &Path) -> anyhow::Result<()> {
    if vcs
        .has_commit_identity(dir)
        .context("failed to read the commit identity")?
    {
        return Ok(());
    }
    Err(crate::untokened!(
        Owed,
        "`rwv init` commits the files it writes, and no author is configured for that \
         commit. Nothing was written. Configure one (`{}`), then run it again.",
        vcs.hint(crate::vcs::VcsAction::SetCommitIdentity)
    ))
}

/// Make the cloned repo at `project_dir`, which holds no `rwv.toml`, a project:
/// write the skeleton `rwv.toml` and the lock driver's attribute line, and
/// commit those and nothing else.
pub(crate) fn make_cloned_repo_a_project(
    vcs: &dyn crate::vcs::Vcs,
    project_dir: &Path,
    source: &str,
) -> anyhow::Result<()> {
    std::fs::write(project_dir.join(Manifest::FILE_NAME), Manifest::SKELETON)
        .with_context(|| format!("failed to write {}", Manifest::FILE_NAME))?;
    let mut written = BTreeSet::from([PathBuf::from(Manifest::FILE_NAME)]);
    if crate::lock_driver::declare_in_working_copy(vcs, project_dir)? {
        written.insert(PathBuf::from(vcs.lock_driver_declaration_file()));
    }
    commit_written(
        vcs,
        project_dir,
        &written,
        &format!("clone --init-manifest {source}"),
    )
}

/// Commit `written` (relative to `project_dir`) and nothing else, as far as
/// each still differs from the committed tree.
fn commit_written(
    vcs: &dyn crate::vcs::Vcs,
    project_dir: &Path,
    written: &BTreeSet<PathBuf>,
    message: &str,
) -> anyhow::Result<()> {
    let dirty: BTreeSet<PathBuf> = vcs
        .dirty_file_names(project_dir)
        .context("failed to read project repo status")?
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let paths: Vec<String> = written
        .intersection(&dirty)
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let committed = vcs
        .stage_paths(project_dir, &refs)
        .map_err(anyhow::Error::from)
        .and_then(|()| {
            crate::repin::commit(
                vcs,
                project_dir,
                &refs,
                message,
                None,
                &crate::repin::Naming::Unnamed,
            )
        });
    match committed {
        Ok(true) => {
            eprintln!("Committed {}", paths.join(", "));
            Ok(())
        }
        Ok(false) => Ok(()),
        Err(refused) => Err(refused.context(format!(
            "the commit was refused: {} {} written in {} and not committed, and the project \
             is not activated",
            paths.join(", "),
            if paths.len() == 1 { "is" } else { "are" },
            project_dir.display()
        ))),
    }
}

/// Record the branch the new project repo is on as `declared`, unless a record
/// already says how it is used.
fn record_trunk(
    vcs: &dyn crate::vcs::Vcs,
    name: &crate::manifest::ProjectName,
    project_dir: &Path,
) -> anyhow::Result<()> {
    crate::branch::record_trunk(vcs, name, project_dir).with_context(|| {
        format!(
            "could not record the branch {} is on; once the cause is fixed, run \
             `rwv branch set-mode <branch> declared` in it",
            project_dir.display()
        )
    })
}

/// The remote URL `--provider <registry>/<owner>` names for project `name`.
fn provider_remote_url(provider: &str, name: &str) -> anyhow::Result<CloneUrl> {
    let (registry_name, owner) = provider.split_once('/').ok_or_else(|| {
        refusal(
            RefusalKind::MalformedProvider,
            format!(
                "invalid --provider format '{provider}', expected 'registry/owner' \
                 (e.g., 'github/myorg')"
            ),
        )
    })?;

    // Look up the registry to get the clone URL pattern. Parse into
    // `RegistryName` at the boundary so the comparison goes through the
    // newtype's PartialEq — any future normalization (case, prefixes)
    // applies here automatically.
    let target = crate::registry::RegistryName::new(registry_name);
    let registries = builtin_registries();
    let registry = registries
        .iter()
        .find(|r| r.name() == &target)
        .ok_or_else(|| {
            let names = crate::registry::builtin_registry_names();
            let known: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
            refusal(
                RefusalKind::UnknownRegistry,
                format!(
                    "unknown registry '{registry_name}'. Known registries: {}",
                    known.join(", ")
                ),
            )
        })?;

    registry
        .clone_url(&RepoId::new(owner, name))
        .ok_or_else(|| {
            refusal(
                RefusalKind::ProviderCannotMintUrl,
                format!(
                    "'{registry_name}' cannot name a repository from an owner and a project \
                 name alone. Create the project without --provider (`rwv init {name}`), \
                 then set the remote once the repo exists."
                ),
            )
        })
}
