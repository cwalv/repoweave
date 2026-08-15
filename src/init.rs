//! Project initialization: `rwv init`.
//!
//! Creates a new project directory under `projects/`, runs `git init`,
//! writes an empty `rwv.toml`, and auto-activates the project. Optionally
//! configures a git remote when `--provider` is given.
//!
//! `rwv init --adopt SOURCE` clones an existing repo as a project. The source
//! can be a URL or a shorthand (`owner/repo` or `registry/owner/repo`). The
//! cloned repo is placed under `projects/{name}/`, an `rwv.toml` is written if
//! missing, and the project is activated.
//!
//! ## Empty-directory bootstrap
//!
//! When invoked in an empty directory (no workspace markers), `rwv init`
//! creates the minimal workspace skeleton (`projects/`) before proceeding.
//! This makes the standard day-0 flow work immediately:
//!
//! ```text
//! mkdir my-ws && cd my-ws
//! rwv init my-project        # bootstraps projects/ and initialises the project
//! rwv add <url>              # works — workspace context resolves
//! ```
//!
//! Running `rwv init` in a non-empty, non-workspace directory is refused with
//! a clear actionable error.

use crate::git::GIT_DEFAULT_REMOTE_NAME;
use crate::manifest::{LockFile, Manifest, RepoUrl};
use crate::registry::{builtin_registries, resolve_to_clone_info, RepoId};
use crate::vcs::project_vcs;
use crate::workspace::{
    create_identity_dir, enclosing_project, project_dir, projects_dir, require_workspace_or_empty,
    warn_confusable_project_siblings, MintedDir, WorkspaceContext,
};
use anyhow::Context;
use std::path::Path;

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
        anyhow::anyhow!(
            "`rwv init` requires either an existing workspace or an empty directory; \
             {} is not a workspace and is not empty. \
             `rwv init` would create a workspace skeleton (projects/) and initialise a project. \
             To proceed: run `rwv init` in an empty directory, or `cd` into an existing workspace.",
            cwd.display()
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
/// - Activates the project (writes `.rwv-active` and generates ecosystem files).
pub fn init(name: &str, provider: Option<&str>, origin_dir: &Path) -> anyhow::Result<()> {
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
        anyhow::bail!(
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
        anyhow::bail!(
            "cannot create project '{name}': {}. Choose another name, or work \
             in the project that is already there.",
            occupant.describe()
        );
    }
    warn_confusable_project_siblings(ctx.primary_path(), name);

    let vcs = project_vcs();
    vcs.init_repo(&project_dir)
        .with_context(|| format!("failed to create a repo in {}", project_dir.display()))?;

    // The skeleton is captured by the initial `git commit` the
    // workweave-create pre-flight requires.
    std::fs::write(project_dir.join(Manifest::FILE_NAME), Manifest::SKELETON)
        .with_context(|| format!("failed to write {}", Manifest::FILE_NAME))?;

    // Configure replay-exclusion for `rwv.lock` so future `rwv sync` runs
    // rebase the project repo natively without dragging the lock through
    // the merge inputs (the lock is regenerated by Phase 3).
    vcs.set_replay_exclusion(&project_dir, Path::new(LockFile::FILE_NAME))
        .context("failed to write .gitattributes")?;
    // Plant the durable `merge.rwv-ours.*` config in the fresh project
    // repo so bare `git rebase --continue` after a partial-conflict resume
    // finds the driver defined (see `plant_rwv_merge_driver_config`).
    // `sync::verify_replay_exclusion_invariant` also self-heals this, but
    // planting at init means even a `git rebase` run outside sync's
    // wrapper is armed from day one.
    crate::git::plant_rwv_merge_driver_config(&project_dir)
        .context("failed to plant merge.rwv-ours config")?;

    // Set up remote from --provider
    if let Some(provider_str) = provider {
        let (registry_name, owner) = provider_str.split_once('/').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --provider format '{}', expected 'registry/owner' (e.g., 'github/myorg')",
                provider_str
            )
        })?;

        // Look up the registry to get the clone URL pattern. Parse into
        // `RegistryName` at the boundary so the comparison goes through the
        // newtype's PartialEq — any future normalisation (case, prefixes)
        // applies here automatically. (Audit A2.)
        let target = crate::registry::RegistryName::new(registry_name);
        let registries = builtin_registries();
        let registry = registries
            .iter()
            .find(|r| r.name() == &target)
            .ok_or_else(|| {
                let names = crate::registry::builtin_registry_names();
                let known: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
                anyhow::anyhow!(
                    "unknown registry '{}'. Known registries: {}",
                    registry_name,
                    known.join(", ")
                )
            })?;

        let repo_id = RepoId::new(owner, name);

        let url = registry.clone_url(&repo_id).ok_or_else(|| {
            anyhow::anyhow!("registry '{}' does not support clone URLs", registry_name)
        })?;

        vcs.add_remote(&project_dir, GIT_DEFAULT_REMOTE_NAME, &url.to_string())
            .with_context(|| {
                format!(
                    "failed to add the {GIT_DEFAULT_REMOTE_NAME} remote in {}",
                    project_dir.display()
                )
            })?;
    }

    eprintln!(
        "Initialized project '{}' at {}",
        name,
        project_dir.display()
    );

    // Auto-activate the newly created project.
    crate::activate::activate(name, &ctx)?;

    Ok(())
}

/// Adopt an existing repo as a project.
///
/// `source` is a URL or shorthand (`owner/repo` or `registry/owner/repo`).
/// The function:
/// 1. Bootstraps a workspace skeleton if `cwd` is an empty directory.
/// 2. Resolves the workspace root from `cwd`.
/// 3. Determines the clone URL and project name from `source`.
/// 4. Clones the repo to `projects/{name}/` (skips if already exists).
/// 5. Writes an empty `rwv.toml` if the clone does not already contain one.
/// 6. Activates the project.
pub fn init_adopt(source: &str, origin_dir: &Path) -> anyhow::Result<()> {
    // Same bootstrap-then-first-resolve pattern as `init` above.
    bootstrap_workspace_if_empty(origin_dir)?;
    let ctx = WorkspaceContext::resolve_invocation(origin_dir, None)?;
    let root = ctx.primary_path();

    // Resolve the source to a clone URL and project name.
    let (clone_url, project_name) = resolve_adopt_source(source)?;

    let project_dir = project_dir(root, &project_name);

    // Collision check
    if project_dir.exists() {
        anyhow::bail!(
            "project '{}' already exists at {}",
            project_name,
            project_dir.display()
        );
    }

    // Clone the repo
    let vcs = project_vcs();
    eprintln!("Cloning {} into {}", clone_url, project_dir.display());
    vcs.clone_repo(&clone_url.to_string(), &project_dir)
        .with_context(|| format!("failed to clone {}", clone_url))?;

    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        std::fs::write(&manifest_path, Manifest::SKELETON)
            .with_context(|| format!("failed to write {}", Manifest::FILE_NAME))?;
    }

    // Configure replay-exclusion for `rwv.lock`. Idempotent — adopted repos
    // that already carry the entry are a no-op. Adopted repos on the
    // legacy `merge=ours` spelling get migrated in place by
    // `set_replay_exclusion`. Also plant the durable `merge.rwv-ours.*`
    // config; see `plant_rwv_merge_driver_config`.
    vcs.set_replay_exclusion(&project_dir, Path::new(LockFile::FILE_NAME))
        .context("failed to write .gitattributes")?;
    crate::git::plant_rwv_merge_driver_config(&project_dir)
        .context("failed to plant merge.rwv-ours config")?;

    eprintln!(
        "Adopted project '{}' at {}",
        project_name,
        project_dir.display()
    );

    // Activate the project
    crate::activate::activate(&project_name, &ctx)?;

    Ok(())
}

/// Resolve an adopt source (URL or shorthand) into a clone URL and project name.
///
/// For full URLs, the project name is derived from the last path segment.
/// For shorthands, the registry is used to construct the clone URL and the
/// repo name becomes the project name.
fn resolve_adopt_source(source: &str) -> anyhow::Result<(RepoUrl, String)> {
    let parsed: RepoUrl = source.parse()?;
    let info = resolve_to_clone_info(&parsed)?;
    let repo = info.id.repo().to_owned();
    Ok((info.url, repo))
}
