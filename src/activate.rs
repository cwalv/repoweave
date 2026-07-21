//! Activate and deactivate projects.
//!
//! `rwv activate PROJECT` sets the active project by:
//! 1. (Intent-verb path only) Running integrations with
//!    `output_dir = project_dir` and `workspace_root = root` to author the
//!    managed/generated files. Context-verb callers skip this step and
//!    instead call `run_verifications` (drift check only — never authors).
//! 2. Collecting the union of `generated_files()` and `managed_files()`
//!    from each enabled integration. This union is the **owner-scoped**
//!    surfacing set used both for symlink creation and for the removal
//!    predicate.
//! 3. Removing old symlinks (from a previous activation) using an
//!    **owner-scoped** predicate: a root symlink is unlinked only if its
//!    name is in the union AND `read_link` resolves to
//!    `projects/<some-project>/<that-file>`. This replaces the previous
//!    blanket "target contains a `projects` component" check, which
//!    swept up unrelated symlinks (e.g. workweave links into source-root
//!    paths under a `projects/` ancestor).
//! 4. Creating new symlinks at the workspace root pointing to the owned
//!    files in the project directory.
//! 5. Writing `.rwv-active`.
//!
//! See [`trigger-model.md`](../docs/repoweave/integration-ownership/trigger-model.md)
//! for the intent-vs-context-verb split (Mode::Intent regenerates and
//! commits content; Mode::Context surfaces+verifies only).
//!
//! `deactivate` removes the symlinks and `.rwv-active`.

use std::collections::BTreeSet;
use std::path::Path;

use crate::integration::{is_enabled, Integration, IntegrationContext, Severity};
use crate::integration_runner::{
    build_detection_cache, run_activate_hooks, run_activations, run_checks, run_verifications,
};
use crate::integrations::builtin_integrations;
use crate::manifest::{IntegrationConfig, Manifest, ProjectName};
use crate::registry::builtin_registries;
use crate::workspace::{set_active_project, Checkout, WorkspaceContext, WorkspaceSession};

/// Which class of verb is driving activation.
///
/// See [`trigger-model.md`](../docs/repoweave/integration-ownership/trigger-model.md):
/// regeneration is a function of *committed intent*, performed by — and
/// committed with — the verb that changes that intent. It is never a side
/// effect of switching context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationMode {
    /// Intent verbs (`rwv add`, `rwv remove`, `rwv update`, `rwv lock`,
    /// and the `rwv doctor --fix` recovery hatch). The active project's
    /// integrations regenerate their managed/generated content; the
    /// resulting files are expected to be committed alongside the
    /// `rwv.yaml` / `rwv.lock` change that motivated the verb.
    Intent,
    /// Context verbs (`rwv activate`, `rwv fetch`, workweave-create).
    /// Surfacing (symlink creation/repair) runs unconditionally; the
    /// integrations' `verify()` pass reports drift between on-disk
    /// content and what `activate()` would produce — but **never
    /// authors content**.
    Context,
}

/// Report integration issues to stderr and bail if any are `Severity::Error`.
///
/// `run_activations` runs BEFORE any symlinks are touched, so an error here
/// means the project has not yet been put into the partially-activated state
/// the comment downstream warns about. Continuing past an integration error
/// would leave `.rwv-active` set without the files the integration was
/// supposed to produce. Audit B2; Ousterhout: don't define errors away by
/// logging-and-continuing. Warnings stay warnings.
fn report_and_check_activation_issues(issues: &[crate::integration::Issue]) -> anyhow::Result<()> {
    let mut error_count = 0usize;
    for issue in issues {
        let prefix = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => {
                error_count += 1;
                "error"
            }
        };
        eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
    }
    if error_count > 0 {
        anyhow::bail!(
            "activate: {error_count} integration error(s); aborting before any symlinks change"
        );
    }
    Ok(())
}

/// Run `rwv activate PROJECT` against an already-resolved context.
///
/// `rwv activate` is a **context verb** (per `trigger-model.md`): it surfaces
/// the existing on-disk artifacts and verifies them, **never authoring**.
/// Runs integration activate hooks (`npm install`, `uv sync`, etc.) by
/// default. See [`activate_with_options`] to suppress them.
pub fn activate(project: &str, ctx: &WorkspaceContext) -> anyhow::Result<()> {
    activate_with_options(project, ctx, ActivateOptions::default())
}

/// Run `rwv activate PROJECT` in **intent mode** — used by `rwv add`,
/// `rwv remove`, `rwv update` after they mutate the manifest. Integration
/// content is (re)authored so the resulting files can be committed alongside
/// the `rwv.yaml` / `rwv.lock` change that motivated the verb.
///
/// See [`trigger-model.md`](../docs/repoweave/integration-ownership/trigger-model.md).
pub fn activate_intent(project: &str, ctx: &WorkspaceContext) -> anyhow::Result<()> {
    activate_intent_with_options(project, ctx, ActivateOptions::default())
}

/// Run intent-mode activation with explicit options. Used by tests that need
/// to drive the write path without running install hooks (the test
/// equivalent of `rwv add --no-install` if that existed).
pub fn activate_intent_with_options(
    project: &str,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    activate_at(
        ctx.primary_path(),
        project,
        false,
        opts,
        ActivationMode::Intent,
    )
}

/// Options for [`activate_with_options`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ActivateOptions {
    /// When true, skip integration activate hooks (install commands like
    /// `npm install`). Used by `rwv activate --no-install` for fast
    /// context-switches.
    pub no_install: bool,
}

/// Run activate with options. Public so the CLI can pass `--no-install`.
pub fn activate_with_options(
    project: &str,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    // Guard: activate has no meaning inside a workweave. The project is fixed
    // at creation time (`rwv workweave <project> create <name>`), so there is
    // no project switch to make. Silently operating on primary from inside a
    // workweave (the status-quo before this fix) was surprising and unsafe —
    // it mutated primary's .rwv-active and weave-root symlinks as a side
    // effect of a command run from an unrelated workweave.
    if let Checkout::Workweave { .. } = &ctx.checkout {
        anyhow::bail!(
            "rwv activate has no effect in a workweave (project is fixed at creation). \
             cd to primary ({}) and rerun.",
            ctx.primary_path().display()
        );
    }

    activate_at(
        ctx.primary_path(),
        project,
        false,
        opts,
        ActivationMode::Context,
    )
}

/// Shared activation logic.
///
/// `skip_missing_sources`: when `true`, symlinks whose source file does not yet
/// exist on disk are skipped (used for workweave activation). When `false`,
/// dangling symlinks are created intentionally so that lock files written by
/// ecosystem tools (Cargo.lock, package-lock.json, …) flow back through the
/// symlink into the project directory.
///
/// `mode`: which class of verb is driving activation (see [`ActivationMode`]).
/// In `Intent` mode the integrations' `activate()` is called (regenerate and
/// commit). In `Context` mode the integrations' `verify()` is called instead
/// (surface + verify, never author).
fn activate_at(
    root: &Path,
    project: &str,
    skip_missing_sources: bool,
    opts: ActivateOptions,
    mode: ActivationMode,
) -> anyhow::Result<()> {
    let project_name = ProjectName::new(project);
    let project_dir = root.join("projects").join(project);
    let manifest_path = project_dir.join("rwv.yaml");
    let manifest = Manifest::from_path(&manifest_path)?;

    // Discover repos on disk and project paths (needed by IntegrationContext).
    let session = WorkspaceSession::new(root);

    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();

    // 1. Integration content step.
    //    Intent verbs (`add`/`remove`/`update`/`lock` paths) author/regenerate
    //    integration content; the generated files are expected to be
    //    committed alongside the rwv.yaml / rwv.lock change that caused the
    //    verb. Context verbs (`activate`/`fetch`/workweave-create) skip
    //    authoring and run the integrations' `verify()` pass instead — they
    //    surface and report drift, but never write content. See
    //    `trigger-model.md`.
    let detection_cache = build_detection_cache(root, manifest.iter_entries());
    let ctx_base = session.context_base(
        &project_dir,
        &project_name,
        &detection_cache,
        manifest.workweave.as_ref(),
    );

    match mode {
        ActivationMode::Intent => {
            let issues = run_activations(&integrations, &manifest, &ctx_base);
            report_and_check_activation_issues(&issues)?;
        }
        ActivationMode::Context => {
            // Two passes:
            //   - run_checks: environment/config preconditions (cargo not on
            //     PATH, declared static file missing, etc.). Same call as
            //     `rwv doctor`. Surfaces as warnings; does not bail.
            //   - run_verifications: drift between intent (rwv.yaml/.lock)
            //     and on-disk managed/generated content. Empty by default;
            //     ports override `verify()` to opt in.
            // Context verbs **never author content**, so even Severity::Error
            // findings are surfaced as warnings and the activation proceeds —
            // the recovery hatch is `rwv doctor --fix`. Bailing here would
            // defeat the spec's unqualified "activate never authors" contract.
            let check_issues = run_checks(&integrations, &manifest, &ctx_base);
            for issue in &check_issues {
                let prefix = match issue.severity {
                    Severity::Warning => "warning",
                    Severity::Error => "warning",
                };
                eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
            }
            let verify_issues = run_verifications(&integrations, &manifest, &ctx_base);
            for issue in &verify_issues {
                let prefix = match issue.severity {
                    Severity::Warning => "warning (drift)",
                    Severity::Error => "warning (drift)",
                };
                eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
            }
        }
    }

    // 2-4. Surface the owner-scoped symlink set. This is the framework
    //    surfacing primitive (`surface_symlinks`): compute the
    //    `generated_files() ∪ managed_files()` union, remove stale
    //    owner-scoped symlinks, and (re)create the symlinks at `root`
    //    pointing into `projects/<project>/`. It is the step-2 path that
    //    workweave-create also runs, and is re-runnable on its own — it
    //    does NOT write `.rwv-active` (project SELECTION, a primary-only
    //    step-1 concept) and does NOT author integration content.
    surface_symlinks(root, &project_name, &manifest, skip_missing_sources)?;

    // 5. Run integration activate hooks (install commands).
    //    Per-integration hooks operate on the now-in-place symlinks at the
    //    workspace root (e.g., `npm install` reads the symlinked
    //    package.json). Suppressed by `--no-install` for fast
    //    context-switches; the user can run install commands directly when
    //    they need them.
    if !opts.no_install {
        let hook_issues = run_activate_hooks(&integrations, &manifest, &ctx_base);
        report_and_check_activate_hook_issues(&hook_issues)?;
    }

    // 6. Write .rwv-active.
    set_active_project(root, &project_name)?;

    Ok(())
}

/// Report integration activate-hook issues to stderr and bail if any are
/// `Severity::Error`.
///
/// Treats activate-hook errors like generated-config errors: a single hook
/// failure (e.g., `npm install` errored out) means the workspace is not
/// fully ready to use, and `.rwv-active` should not record success.
/// Warnings stay warnings.
fn report_and_check_activate_hook_issues(
    issues: &[crate::integration::Issue],
) -> anyhow::Result<()> {
    let mut error_count = 0usize;
    for issue in issues {
        let prefix = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => {
                error_count += 1;
                "error"
            }
        };
        eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
    }
    if error_count > 0 {
        anyhow::bail!(
            "activate: {error_count} integration activate-hook error(s); \
             workspace may be partially activated — \
             run `rwv doctor --fix` to repair symlinks and re-run install hooks"
        );
    }
    Ok(())
}

/// Remove activation symlinks at the workspace root (recursively), restricted
/// to **owner-scoped** entries.
///
/// `owned_files` is the union of `generated_files()` and `managed_files()`
/// across all currently-enabled integrations (typically the project being
/// activated). A root symlink is unlinked iff:
///
/// 1. Its root-relative path is in `owned_files`, AND
/// 2. Its `read_link` target resolves to `projects/<some-project>/<that-file>`
///    (any project; the `projects/` ancestor + matching tail is the owner
///    proof).
///
/// This replaces the previous blanket "target has a `projects` component"
/// check, which swept up unrelated symlinks (e.g. workweave links whose
/// resolved source-root path happens to live under a `projects/` ancestor —
/// the rwv-c5h surfacing-layer concern at the framework level). Directories
/// that were created solely to hold nested symlinks are cleaned up if they
/// become empty.
fn remove_activation_symlinks(root: &Path, owned_files: &BTreeSet<String>) -> anyhow::Result<()> {
    remove_activation_symlinks_in(root, root, owned_files)
}

/// True if `target` (the `read_link` output of a symlink at `link_path`,
/// where `link_path` is a descendant of `root`) names a file owned by the
/// current activation: the target must have one of the forms
///
///   - `projects/<project>/<rel>`  (top-level symlink)
///   - `../projects/<project>/<dir>/<rel>` etc. (nested symlink)
///
/// and `<rel>` (joined back from the `projects/<project>/` boundary to the
/// end of the target) must equal `rel_from_root` — the root-relative path
/// of the symlink itself. This proves the symlink is the surfacing of a
/// project-owned file at the expected location.
///
/// The relative tail comparison is what makes the predicate owner-scoped:
/// a symlink at `root/foo` pointing at `projects/p/bar` is NOT owned
/// surfacing of `foo` (the owner would have produced `projects/p/foo`).
fn target_resolves_to_projects(rel_from_root: &Path, target: &Path) -> bool {
    let mut comps = target.components().peekable();
    // Skip any leading parent-dir components (`../../...` for nested links).
    while let Some(c) = comps.peek() {
        if c.as_os_str() == ".." {
            comps.next();
        } else {
            break;
        }
    }
    // Expect `projects` next.
    match comps.next() {
        Some(c) if c.as_os_str() == "projects" => {}
        _ => return false,
    }
    // Skip the project segment (one component).
    if comps.next().is_none() {
        return false;
    }
    // Whatever remains is the file path under `projects/<project>/`.
    let tail: std::path::PathBuf = comps.collect();
    tail == rel_from_root
}

fn remove_activation_symlinks_in(
    dir: &Path,
    root: &Path,
    owned_files: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match path.symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                // Owner-scoped predicate (§4.1 of the integration-ownership
                // plan): unlink only when both (a) the symlink's name is in
                // the active integrations' union, AND (b) its target resolves
                // to `projects/<some-project>/<that-file>`. A symlink whose
                // name we don't claim, or whose target points elsewhere
                // (e.g. workweave.link → source-root path), is preserved.
                let rel_from_root = match path.strip_prefix(root) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let rel_str = rel_from_root.to_string_lossy();
                let in_owned_set = owned_files.contains(rel_str.as_ref());
                let resolves_to_projects = target_resolves_to_projects(rel_from_root, &target);
                if in_owned_set && resolves_to_projects {
                    std::fs::remove_file(&path)?;
                }
            }
        } else if meta.file_type().is_dir() {
            // Skip well-known workspace directories to avoid unnecessary
            // recursion. The set of registry directory names is the canonical
            // source — open-coding it here means a new registry (e.g.
            // codeberg) added to `registry.rs` would silently recurse into
            // a registry tree. Derive from `builtin_registries()` + the
            // workspace constants. Audit A3.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                let is_registry_dir = builtin_registries()
                    .iter()
                    .any(|r| r.name().as_str() == name);
                if is_registry_dir || name == "projects" || name == ".git" {
                    continue;
                }
            }
            remove_activation_symlinks_in(&path, root, owned_files)?;

            // Clean up empty directories that we may have created.
            if dir != root {
                let _ = std::fs::remove_dir(dir); // ignore error if not empty
            }
        }
    }

    // If we're in a subdirectory, try to remove it if empty.
    if dir != root {
        let _ = std::fs::remove_dir(dir);
    }

    Ok(())
}

/// Run activation in a workweave directory without calling resolve.
///
/// This is used by `create_workweave` after the workweave is fully set up.
/// Unlike `activate`, it does not call `WorkspaceContext::resolve` (which would
/// return the primary root via the `.rwv-workweave` marker). Instead it works
/// directly against the workweave directory.
///
/// Symlinks for files that do not yet exist on disk are skipped (the workweave
/// is a view onto an existing project, so dangling symlinks are not useful).
///
/// Install hooks (`npm install`, `cargo generate-lockfile`, …) are
/// skipped at workweave creation: the workweave shares clones with
/// primary, so install state is typically inherited rather than
/// regenerated. The user can run `rwv activate --reinstall`-style
/// commands inside the workweave when they actually need a refresh.
///
/// Workweave-create is a **context verb** (per `trigger-model.md`): the
/// generated files were already authored in the project repo by an earlier
/// intent verb, so the workweave only needs to surface them via symlinks
/// and verify drift — not re-author.
pub fn activate_workweave(project: &str, workweave_dir: &Path) -> anyhow::Result<()> {
    activate_at(
        workweave_dir,
        project,
        true,
        ActivateOptions { no_install: true },
        ActivationMode::Context,
    )
}

/// Run **intent-mode** activation inside a workweave. Called by
/// [`crate::add_remove::run_add`] / `run_remove` and by `rwv update` when
/// CWD is a workweave: the manifest change just landed and the integrations
/// must regenerate their managed content so it can be committed alongside.
///
/// Symlinks for files that do not yet exist on disk are skipped (the
/// workweave is a view onto an existing project, so dangling symlinks are
/// not useful). Install hooks remain suppressed in the workweave (mirroring
/// [`activate_workweave`]).
pub fn activate_workweave_intent(project: &str, workweave_dir: &Path) -> anyhow::Result<()> {
    activate_at(
        workweave_dir,
        project,
        true,
        ActivateOptions { no_install: true },
        ActivationMode::Intent,
    )
}

/// Deactivate the current project: remove symlinks and `.rwv-active`.
///
/// Computes the owner-scoped union the same way `activate_at` does, then
/// removes only symlinks claimed by the just-deactivated project's
/// integration set. Symlinks the framework doesn't own (e.g. user-created
/// workweave links to source-root paths) are preserved.
#[allow(dead_code)]
pub fn deactivate(ctx: &WorkspaceContext) -> anyhow::Result<()> {
    let root = ctx.primary_path();

    let owned = compute_active_owned_set(root)?;
    remove_activation_symlinks(root, &owned)?;

    let active_file = root.join(".rwv-active");
    if active_file.exists() {
        std::fs::remove_file(&active_file)?;
    }

    Ok(())
}

/// Compute the owner-scoped surfacing set for `project` at `root`: the union
/// of `generated_files()` and `managed_files()` across all enabled
/// integrations, mapping each file to the integration that declares it.
///
/// This is the single source of truth for the surfacing union — the set of
/// root-relative paths the framework symlinks for a project. Its consumers are
/// the surfacing primitive ([`surface_symlinks`]) and the framework surfacing
/// check ([`verify_surfacing`]). When a path is declared by more than one
/// enabled integration the first declarer wins for the label; the path itself
/// is coalesced (the set is keyed by path).
///
/// Returns `(path -> declaring-integration-name)`. Iteration order is sorted
/// by path (`BTreeMap`), matching the deterministic ordering the previous
/// `BTreeSet` provided to symlink creation.
fn compute_owned_set(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> std::collections::BTreeMap<String, String> {
    let project_dir = root.join("projects").join(project.as_str());
    let session = WorkspaceSession::new(root);
    let detection_cache = build_detection_cache(root, manifest.iter_entries());
    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let default_config = IntegrationConfig::default();

    let mut owned: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for integration in &integrations {
        let config = manifest
            .integrations
            .get(integration.name())
            .unwrap_or(&default_config);
        if !is_enabled(*integration, config) {
            continue;
        }
        let int_ctx = IntegrationContext {
            output_dir: &project_dir,
            workspace_root: root,
            project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config,
            all_repos_on_disk: session.repos_on_disk(),
            all_project_paths: session.project_paths(),
            detection_cache: &detection_cache,
            workweave: manifest.workweave.as_ref(),
        };
        for f in integration
            .generated_files(&int_ctx)
            .into_iter()
            .chain(integration.managed_files(&int_ctx))
        {
            owned
                .entry(f)
                .or_insert_with(|| integration.name().to_string());
        }
    }
    owned
}

/// Compute the owner-scoped surfacing set for the currently-active project,
/// reading `.rwv-active` from `root`. Returns an empty set if no project is
/// active (in which case no symlinks are owned by rwv and nothing gets
/// removed). This is the deactivate-side analogue of step 2 in
/// [`activate_at`].
fn compute_active_owned_set(root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let active = match crate::workspace::read_active_project(root) {
        Some(name) => name,
        None => return Ok(BTreeSet::new()),
    };
    let manifest_path = root.join("projects").join(active.as_str()).join("rwv.yaml");
    if !manifest_path.exists() {
        return Ok(BTreeSet::new());
    }
    let manifest = Manifest::from_path(&manifest_path)?;
    Ok(compute_owned_set(root, &active, &manifest)
        .into_keys()
        .collect())
}

/// Surface the owner-scoped symlink set for `project` into `root` (the
/// **step-2 surfacing primitive**, factored out of [`activate_at`]).
///
/// This is the re-runnable framework primitive that:
///  1. Computes the `generated_files() ∪ managed_files()` union
///     ([`compute_owned_set`]).
///  2. Removes stale owner-scoped symlinks (union of the new set with the
///     previously-active project's set, each verified to resolve to
///     `projects/<some-project>/<rel>`).
///  3. Creates the symlinks at `<root>/<file>` pointing at
///     `projects/<project>/<file>`.
///
/// It does **not** write `.rwv-active` (that is step-1 project SELECTION, a
/// primary-only concept) and does **not** author integration content. Because
/// it is bound to `root` (the CWD weave directory) rather than to primary, it
/// is valid in any weave — it is exactly what workweave-create runs at
/// creation, and what `rwv doctor --fix` re-runs to repair missing surfacing.
///
/// `skip_missing_sources`: when `true`, symlinks whose source file does not yet
/// exist are skipped (workweave surfacing — a view onto an existing project).
/// When `false`, dangling symlinks are created intentionally so ecosystem lock
/// files written later flow back through the symlink into the project dir.
pub fn surface_symlinks(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    skip_missing_sources: bool,
) -> anyhow::Result<()> {
    let project_dir = root.join("projects").join(project.as_str());

    // 1. Collect the owner-scoped surfacing set.
    let owned = compute_owned_set(root, project, manifest);
    let new_owned: BTreeSet<String> = owned.keys().cloned().collect();
    let new_generated: Vec<String> = new_owned.iter().cloned().collect();

    // 2. Remove old symlinks from a previous activation using the
    //    owner-scoped predicate: a root symlink is unlinked only if its
    //    name is in the **removal candidate set** AND `read_link` resolves
    //    to `projects/<some-project>/<that-file>`.
    //
    //    The candidate set is the UNION of:
    //      - `new_owned` — the new project's integration outputs, and
    //      - the previously-active project's owned set (read .rwv-active,
    //        load its manifest, recompute) — without this, switching A→B
    //        leaves orphaned symlinks for integrations B doesn't enable.
    let removal_candidates = {
        let mut union = new_owned.clone();
        if let Ok(prev_owned) = compute_active_owned_set(root) {
            for f in prev_owned {
                union.insert(f);
            }
        }
        union
    };
    remove_activation_symlinks(root, &removal_candidates)?;

    // 3. Create new symlinks at root pointing to project_dir files.
    //    Failures are collected as warnings so that partial symlink creation
    //    does not prevent the caller from proceeding.
    for file in &new_generated {
        let source = project_dir.join(file);
        let link = root.join(file);

        if skip_missing_sources && !source.exists() {
            continue;
        }

        // When skip_missing_sources is false, create symlinks even if the
        // target doesn't exist yet — lock files (Cargo.lock, package-lock.json,
        // etc.) are populated by ecosystem tools on first build/install,
        // writing through the dangling symlink.

        // Ensure parent directory exists for nested files (e.g., gita/repos.csv).
        if let Some(parent) = link.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!(
                        "[warning] symlink: failed to create parent directory {}: {e}",
                        parent.display()
                    );
                    continue;
                }
            }
        }

        // Compute a relative symlink target from the link location to the
        // source in the project directory. For top-level files this is just
        // `projects/{project}/{file}`. For nested files like `gita/repos.csv`
        // we need to prepend `../` for each directory level.
        let relative_target = relative_symlink_target(project.as_str(), file);

        #[cfg(unix)]
        if let Err(e) = std::os::unix::fs::symlink(&relative_target, &link) {
            eprintln!(
                "[warning] symlink: failed to create {} -> {}: {e}",
                link.display(),
                relative_target.display()
            );
        }

        #[cfg(windows)]
        if let Err(e) = std::os::windows::fs::symlink_file(&relative_target, &link) {
            eprintln!(
                "[warning] symlink: failed to create {} -> {}: {e}",
                link.display(),
                relative_target.display()
            );
        }
    }

    Ok(())
}

/// The relative symlink target a surfaced file should point at, from the link
/// location (`<root>/<file>`) to the source (`projects/<project>/<file>`).
///
/// For top-level files this is `projects/<project>/<file>`. For nested files
/// like `gita/repos.csv` we prepend `../` once per directory level so the link
/// remains relative. This is the single source of truth shared by symlink
/// creation ([`surface_symlinks`]) and the surfacing check
/// ([`verify_surfacing`]) so the "what target should exist" question has one
/// answer.
fn relative_symlink_target(project: &str, file: &str) -> std::path::PathBuf {
    let depth = Path::new(file)
        .parent()
        .map(|p| p.components().count())
        .unwrap_or(0);
    let mut relative_target = std::path::PathBuf::new();
    for _ in 0..depth {
        relative_target.push("..");
    }
    relative_target.push("projects");
    relative_target.push(project);
    relative_target.push(file);
    relative_target
}

/// Framework-level **Axis-1 surfacing** check: assert that every file in the
/// owner-scoped surfacing union exists at `<root>/<file>` as a symlink that
/// resolves to `projects/<project>/<file>`.
///
/// This is a SECOND CONSUMER of the same `generated_files() ∪ managed_files()`
/// union that drives symlink creation ([`compute_owned_set`]) — it lives in the
/// framework and is byte-identical across all integrations, so it is NOT
/// duplicated into per-integration `verify()` bodies (those own Axis-2 content
/// drift). Any divergence between an integration's declared surfacing set and
/// the on-disk symlinks (manual `rm`, interrupted create, a manifest change
/// that adds a file, enabling an integration in an existing workweave) is
/// invisible to the per-integration verify pass; this closes that gap.
///
/// Emits one `Severity::Warning`, `safe_to_fix=true` `Issue` per missing or
/// mis-resolved symlink. The recovery hatch is `rwv doctor --fix`, which calls
/// [`surface_symlinks`] (NOT `activate_intent` — re-surfacing is valid in any
/// weave, project re-selection is not).
///
/// `skip_missing_sources` mirrors the create path: when `true` (workweave
/// surfacing), a file whose source does not yet exist on disk is NOT expected
/// to be surfaced, so its missing symlink is not flagged — this keeps the
/// check symmetric with what [`surface_symlinks`] actually creates.
pub fn verify_surfacing(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    skip_missing_sources: bool,
) -> Vec<crate::integration::Issue> {
    use crate::integration::{Issue, Severity};

    let project_dir = root.join("projects").join(project.as_str());
    let owned = compute_owned_set(root, project, manifest);
    let mut issues = Vec::new();

    for (file, integration) in &owned {
        let source = project_dir.join(file);
        // Mirror the create path: a file whose source is absent in a workweave
        // is intentionally not surfaced, so don't flag its missing symlink.
        if skip_missing_sources && !source.exists() {
            continue;
        }

        let link = root.join(file);
        let expected_target = relative_symlink_target(project.as_str(), file);

        let link_meta = match link.symlink_metadata() {
            Ok(m) => m,
            Err(_) => {
                // Nothing at the surfacing location at all.
                issues.push(Issue {
                    integration: integration.clone(),
                    severity: Severity::Warning,
                    message: format!(
                        "surfacing: `{file}` is not surfaced (no symlink at `{}`; safe to --fix)",
                        link.display()
                    ),
                    safe_to_fix: true,
                });
                continue;
            }
        };

        if !link_meta.file_type().is_symlink() {
            // A regular file/dir sits where the surfacing symlink should be.
            // This is a real, hand-held divergence — surface it, but flag it
            // as not-safe-to-fix so doctor --fix never clobbers user content
            // sitting at the surfacing path.
            issues.push(Issue {
                integration: integration.clone(),
                severity: Severity::Warning,
                message: format!(
                    "surfacing: `{file}` is not a symlink (a real file/dir occupies `{}`; \
                     not auto-fixed)",
                    link.display()
                ),
                safe_to_fix: false,
            });
            continue;
        }

        match std::fs::read_link(&link) {
            Ok(actual) if actual == expected_target => {
                // Surfaced correctly.
            }
            Ok(actual) => {
                issues.push(Issue {
                    integration: integration.clone(),
                    severity: Severity::Warning,
                    message: format!(
                        "surfacing: `{file}` symlink resolves to `{}` (expected `{}`; safe to --fix)",
                        actual.display(),
                        expected_target.display()
                    ),
                    safe_to_fix: true,
                });
            }
            Err(e) => {
                issues.push(Issue {
                    integration: integration.clone(),
                    severity: Severity::Warning,
                    message: format!(
                        "surfacing: `{file}` symlink unreadable at `{}` ({e}; safe to --fix)",
                        link.display()
                    ),
                    safe_to_fix: true,
                });
            }
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::Issue;

    fn issue(integration: &str, severity: Severity, message: &str) -> Issue {
        Issue {
            integration: integration.into(),
            severity,
            message: message.into(),
            safe_to_fix: true,
        }
    }

    #[test]
    fn report_and_check_no_issues_returns_ok() {
        assert!(report_and_check_activation_issues(&[]).is_ok());
    }

    #[test]
    fn report_and_check_warnings_only_returns_ok() {
        let issues = vec![issue("npm", Severity::Warning, "w1")];
        assert!(report_and_check_activation_issues(&issues).is_ok());
    }

    #[test]
    fn report_and_check_any_error_bails() {
        // B2: a single integration error must bail BEFORE the caller starts
        // touching symlinks. Logging-and-continuing leaves .rwv-active set
        // without the integration's outputs — Ousterhout: don't define
        // errors away by silently degrading.
        let issues = vec![
            issue("cargo", Severity::Warning, "w"),
            issue("npm", Severity::Error, "boom"),
        ];
        let err = report_and_check_activation_issues(&issues).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("1 integration error") && msg.contains("before any symlinks"),
            "expected aggregate error message about integration errors, got: {msg}"
        );
    }

    #[test]
    fn report_and_check_multiple_errors_aggregate_in_message() {
        let issues = vec![
            issue("a", Severity::Error, "e1"),
            issue("b", Severity::Error, "e2"),
            issue("c", Severity::Warning, "w"),
        ];
        let err = report_and_check_activation_issues(&issues).unwrap_err();
        assert!(err.to_string().contains("2 integration error"));
    }

    // -----------------------------------------------------------------------
    // Repair-verb naming (fo-oueuv7.2)
    // -----------------------------------------------------------------------

    #[test]
    fn activate_hook_error_names_doctor_fix() {
        // Partial-activation errors (from install hooks like `npm install`,
        // `cargo generate-lockfile`) must name `rwv doctor --fix` as the
        // repair verb — re-running activate does NOT re-run hooks and cannot
        // self-heal. This test asserts the house pattern:
        //   state → verb → escape hatch.
        let issues = vec![issue("cargo", Severity::Error, "generate-lockfile failed")];
        let err = report_and_check_activate_hook_issues(&issues).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("doctor --fix"),
            "partial-activation error must name `rwv doctor --fix` as the repair verb; \
             got: {msg}"
        );
        assert!(
            msg.contains("partially activated"),
            "partial-activation error must describe the state; got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // relative_symlink_target
    // -----------------------------------------------------------------------

    #[test]
    fn relative_target_top_level_is_projects_project_file() {
        let t = relative_symlink_target("web-app", ".claude");
        assert_eq!(t, Path::new("projects/web-app/.claude"));
    }

    #[test]
    fn relative_target_nested_prepends_parent_dirs() {
        // gita/repos.csv lives one dir deep, so the link must climb out once.
        let t = relative_symlink_target("p", "gita/repos.csv");
        assert_eq!(t, Path::new("../projects/p/gita/repos.csv"));
    }

    // -----------------------------------------------------------------------
    // surface_symlinks + verify_surfacing (Axis-1 surfacing, fo-huwqqc)
    // -----------------------------------------------------------------------

    /// Build a workspace on disk with one project whose static-files
    /// integration declares `files`. Each declared file is created in the
    /// project directory with placeholder content. Returns the workspace root.
    fn make_surfacing_workspace(files: &[&str]) -> (tempfile::TempDir, ProjectName) {
        make_surfacing_workspace_authoring(files, files)
    }

    /// Like [`make_surfacing_workspace`] but `declared` are the files the
    /// static-files integration declares (the surfacing union) while only
    /// `authored` are actually written into the project dir. A declared-but-
    /// not-authored file models a source that does not exist on disk (e.g. a
    /// lockfile not yet generated) — without resorting to a destructive
    /// `remove_file` in test code (the codebase keeps deletes out of `src/`
    /// test modules; see the destructive-ops tripwire).
    fn make_surfacing_workspace_authoring(
        declared: &[&str],
        authored: &[&str],
    ) -> (tempfile::TempDir, ProjectName) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Registry marker dir so WorkspaceSession scans cleanly.
        std::fs::create_dir_all(root.join("github")).unwrap();
        let project = "web-app";
        let project_dir = root.join("projects").join(project);
        std::fs::create_dir_all(&project_dir).unwrap();

        let files_yaml = declared
            .iter()
            .map(|f| format!("      - {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        // Disable the default-enabled integrations that declare files
        // unconditionally (vscode-workspace surfaces `<project>.code-workspace`,
        // go-work surfaces `go.sum`) so the surfacing union under test is
        // exactly the static-files set we declare. The surfacing machinery is
        // integration-agnostic; isolating one integration keeps the asserts
        // deterministic.
        let manifest_yaml = format!(
            "repositories: {{}}\n\
             integrations:\n\
             \x20 static-files:\n\
             \x20   enabled: true\n\
             \x20   files:\n{files_yaml}\n\
             \x20 vscode-workspace:\n\
             \x20   enabled: false\n\
             \x20 go-work:\n\
             \x20   enabled: false\n"
        );
        std::fs::write(project_dir.join("rwv.yaml"), &manifest_yaml).unwrap();

        // Author the requested files in the project dir so they can be surfaced.
        for f in authored {
            let p = project_dir.join(f);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, format!("content of {f}\n")).unwrap();
        }

        (tmp, ProjectName::new(project))
    }

    fn load_manifest(root: &Path, project: &ProjectName) -> Manifest {
        let path = root
            .join("projects")
            .join(project.as_str())
            .join("rwv.yaml");
        Manifest::from_path(&path).unwrap()
    }

    #[test]
    fn surface_then_verify_is_clean() {
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        surface_symlinks(root, &project, &manifest, false).unwrap();

        // The symlink exists at the root and resolves to the project copy.
        let link = root.join(".claude");
        let meta = link.symlink_metadata().unwrap();
        assert!(meta.file_type().is_symlink(), ".claude should be a symlink");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("projects/web-app/.claude")
        );

        // verify_surfacing reports nothing — surfacing matches the union.
        let issues = verify_surfacing(root, &project, &manifest, false);
        assert!(
            issues.is_empty(),
            "expected clean surfacing, got: {issues:?}"
        );
    }

    #[test]
    fn verify_flags_missing_symlink_safe_to_fix() {
        // The motivating case: the union gained a file (e.g. static-files set
        // gained `.claude`) but the symlink was never created in this weave.
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        // Do NOT surface — the symlink is absent.
        let issues = verify_surfacing(root, &project, &manifest, false);
        assert_eq!(issues.len(), 1, "expected one missing-surfacing issue");
        let issue = &issues[0];
        assert_eq!(issue.integration, "static-files");
        assert_eq!(issue.severity, Severity::Warning);
        assert!(issue.safe_to_fix, "missing symlink is safe to --fix");
        assert!(
            issue.message.contains(".claude") && issue.message.contains("not surfaced"),
            "message should name the file and the gap: {}",
            issue.message
        );
    }

    #[test]
    fn verify_flags_mis_resolved_symlink() {
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        // Place a symlink that points somewhere wrong (the owner would have
        // produced `projects/web-app/.claude`). Construct the bad state
        // directly rather than surface-then-delete.
        std::os::unix::fs::symlink("projects/other/.claude", root.join(".claude")).unwrap();

        let issues = verify_surfacing(root, &project, &manifest, false);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].safe_to_fix);
        assert!(
            issues[0].message.contains("resolves to") && issues[0].message.contains("expected"),
            "mis-resolved message should name actual + expected: {}",
            issues[0].message
        );
    }

    #[test]
    fn verify_flags_non_symlink_as_user_held() {
        // A real file sitting at the surfacing path is hand-held content — the
        // owner-scoped removal never touches non-symlinks, so re-surfacing
        // can't repair it. Flag it not-safe-to-fix so --fix won't claim a fix.
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        std::fs::write(root.join(".claude"), "i am a real file\n").unwrap();

        let issues = verify_surfacing(root, &project, &manifest, false);
        assert_eq!(issues.len(), 1);
        assert!(
            !issues[0].safe_to_fix,
            "a real file at the surfacing path must not be auto-fixed"
        );
        assert!(issues[0].message.contains("not a symlink"));
    }

    #[test]
    fn fix_path_re_surfaces_missing_symlink() {
        // End-to-end of the --fix primitive: detect missing → re-surface →
        // verify clean. This is what doctor --fix calls in a workweave.
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        // Start from the un-surfaced (missing) state — the symlink was never
        // created in this weave (manifest gained the file after create, or a
        // manual rm). The check flags it.
        assert_eq!(verify_surfacing(root, &project, &manifest, false).len(), 1);

        // The fix primitive (NOT activate_intent) creates the symlink.
        surface_symlinks(root, &project, &manifest, false).unwrap();
        assert!(
            verify_surfacing(root, &project, &manifest, false).is_empty(),
            "re-surfacing should clear the missing-symlink finding"
        );
    }

    #[test]
    fn surface_does_not_write_rwv_active() {
        // The factored primitive is step-2 ONLY: it must not perform step-1
        // project SELECTION (.rwv-active write), which is the primary-only
        // concept fo-9fnae forbids dragging into a workweave.
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        surface_symlinks(root, &project, &manifest, false).unwrap();
        assert!(
            !root.join(".rwv-active").exists(),
            "surface_symlinks must not write .rwv-active (that is step-1 selection)"
        );
    }

    #[test]
    fn verify_skips_missing_source_in_workweave_mode() {
        // In workweave mode (skip_missing_sources=true), a declared file whose
        // source does not exist on disk is intentionally not surfaced, so its
        // absent symlink must NOT be flagged — the check stays symmetric with
        // what surface_symlinks actually creates.
        //
        // Build the state directly: `.claude` is DECLARED but NOT authored in
        // the project dir, modelling a source that doesn't exist yet.
        let (tmp, project) = make_surfacing_workspace_authoring(&[".claude"], &[]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        // skip_missing_sources = true → no finding (source absent, not surfaced).
        assert!(verify_surfacing(root, &project, &manifest, true).is_empty());
        // skip_missing_sources = false → the missing symlink IS flagged
        // (primary semantics create dangling symlinks for lockfiles etc.).
        assert_eq!(verify_surfacing(root, &project, &manifest, false).len(), 1);
    }
}
