//! Project initialization: `rwv init`.
//!
//! Creates a new project directory under `projects/`, runs `git init`,
//! writes an empty `rwv.toml`, authors the integration files it implies,
//! commits exactly those files as the project's first commit, and
//! auto-activates the project. Optionally configures a git remote when
//! `--provider` is given.
//!
//! `rwv init --adopt SOURCE` clones an existing repo as a project. The source
//! can be a URL or a shorthand (`owner/repo` or `registry/owner/repo`). The
//! cloned repo is placed under `projects/{name}/`, an `rwv.toml` is written if
//! missing, the files it wrote are committed, and the project is activated.
//! It authors nothing: its members are not on disk yet, and authoring from
//! the repos it can see would rewrite the adopted project's committed files
//! without the rest.
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

use crate::manifest::{LockFile, Manifest, RepoUrl};
use crate::refusal::{refusal, RefusalKind};
use crate::registry::{builtin_registries, resolve_to_clone_info, CloneUrl, RepoId};
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
    // subsequent `WorkspaceContext::resolve_invocation` call in `init`/`init_adopt` finds
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

    // Configure replay-exclusion for `rwv.lock` so future `rwv sync` runs
    // rebase the project repo natively without dragging the lock through
    // the merge inputs (the lock is regenerated by Phase 3).
    let replay_exclusion_file = vcs
        .set_replay_exclusion(&project_dir, Path::new(LockFile::FILE_NAME))
        .context("failed to write .gitattributes")?;
    // Plant the durable `merge.rwv-ours.*` config in the fresh project
    // repo so bare `git rebase --continue` after a partial-conflict resume
    // finds the driver defined (see `plant_rwv_merge_driver_config`).
    // `sync::verify_replay_exclusion_invariant` also self-heals this, but
    // planting at init means even a `git rebase` run outside sync's
    // wrapper is armed from day one.
    crate::git::plant_rwv_merge_driver_config(&project_dir)
        .context("failed to plant merge.rwv-ours config")?;

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
    written.extend(replay_exclusion_file);
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
fn judge_commit_identity(vcs: &dyn crate::vcs::Vcs, dir: &Path) -> anyhow::Result<()> {
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
        .and_then(|()| vcs.commit_only(project_dir, &refs, message));
    match committed {
        Ok(true) => {
            eprintln!("Committed {}", paths.join(", "));
            Ok(())
        }
        Ok(false) => Ok(()),
        Err(refused) => Err(anyhow::Error::from(refused).context(format!(
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

/// Adopt an existing repo as a project.
///
/// `source` is a URL or shorthand (`owner/repo` or `registry/owner/repo`).
/// The function:
/// 1. Determines the clone URL and project name from `source`, and
///    validates the name.
/// 2. Bootstraps a workspace skeleton if `cwd` is an empty directory.
/// 3. Resolves the workspace root from `cwd`.
/// 4. Clones the repo to `projects/{name}/`, refusing an occupied directory.
/// 5. Writes an empty `rwv.toml` if the clone does not already contain one,
///    and the replay exclusion, and commits whichever of the two it wrote.
/// 6. Activates the project.
pub fn init_adopt(
    source: &str,
    origin_dir: &Path,
    opts: crate::activate::ActivateOptions,
) -> anyhow::Result<()> {
    // Resolve the source to a clone URL and project name, and validate the
    // name, before anything touches disk. `resolve_adopt_source` is a pure
    // string computation, so this is free; the clone below is not, and a
    // refused name must not leave a partial clone for a retry to trip over.
    let (clone_url, project_name) = resolve_adopt_source(source)?;
    let project_name = crate::manifest::ProjectName::new(project_name)?;
    let vcs = project_vcs();
    judge_commit_identity(vcs.as_ref(), origin_dir)?;

    // Same bootstrap-then-first-resolve pattern as `init` above.
    bootstrap_workspace_if_empty(origin_dir)?;
    let ctx = WorkspaceContext::resolve_invocation(origin_dir, None)?;
    let root = ctx.primary_path();

    let project_dir = project_dir(root, &project_name);

    // Collision check
    if project_dir.exists() {
        crate::refuse!(
            RefusalKind::ProjectDirOccupied,
            "project '{}' already exists at {}",
            project_name,
            project_dir.display()
        );
    }

    eprintln!("Cloning {} into {}", clone_url, project_dir.display());
    let url_remote = vcs
        .clone_repo(&clone_url, &project_dir)
        .with_context(|| format!("failed to clone {}", clone_url))?;
    vcs.record_project_url_remote(&project_dir, &url_remote)
        .with_context(|| {
            format!(
                "failed to record the url remote in {}",
                project_dir.display()
            )
        })?;

    let mut written = BTreeSet::new();
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        std::fs::write(&manifest_path, Manifest::SKELETON)
            .with_context(|| format!("failed to write {}", Manifest::FILE_NAME))?;
        written.insert(PathBuf::from(Manifest::FILE_NAME));
    }

    // Configure replay-exclusion for `rwv.lock`. Idempotent — adopted repos
    // that already carry the entry are a no-op. Adopted repos on the
    // legacy `merge=ours` spelling get migrated in place by
    // `set_replay_exclusion`. Also plant the durable `merge.rwv-ours.*`
    // config; see `plant_rwv_merge_driver_config`.
    written.extend(
        vcs.set_replay_exclusion(&project_dir, Path::new(LockFile::FILE_NAME))
            .context("failed to write .gitattributes")?,
    );
    crate::git::plant_rwv_merge_driver_config(&project_dir)
        .context("failed to plant merge.rwv-ours config")?;
    commit_written(
        vcs.as_ref(),
        &project_dir,
        &written,
        &format!("init --adopt {source}"),
    )?;

    record_trunk(vcs.as_ref(), &project_name, &project_dir)?;
    crate::alignment::record(root, &project_name)?;

    eprintln!(
        "Adopted project '{}' at {}",
        project_name,
        project_dir.display()
    );

    // Activate the project
    crate::activate::activate_with_options(&project_name, &ctx, opts)?;

    Ok(())
}

/// Resolve an adopt source (URL or shorthand) into a clone URL and project name.
///
/// For full URLs, the project name is derived from the last path segment.
/// For shorthands, the registry is used to construct the clone URL and the
/// repo name becomes the project name.
fn resolve_adopt_source(source: &str) -> anyhow::Result<(CloneUrl, String)> {
    let parsed: RepoUrl = source.parse()?;
    let info = resolve_to_clone_info(&parsed)?;
    let repo = info.id.repo().to_owned();
    Ok((info.url, repo))
}
