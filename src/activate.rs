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
use crate::workspace::{set_active_project, WorkspaceContext, WorkspaceLocation, WorkspaceSession};

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

/// Run `rwv activate PROJECT` from the given working directory.
///
/// `rwv activate` is a **context verb** (per `trigger-model.md`): it surfaces
/// the existing on-disk artifacts and verifies them, **never authoring**.
/// Runs integration activate hooks (`npm install`, `uv sync`, etc.) by
/// default. See [`activate_with_options`] to suppress them.
pub fn activate(project: &str, cwd: &Path) -> anyhow::Result<()> {
    activate_with_options(project, cwd, ActivateOptions::default())
}

/// Run `rwv activate PROJECT` in **intent mode** — used by `rwv add`,
/// `rwv remove`, `rwv update` after they mutate the manifest. Integration
/// content is (re)authored so the resulting files can be committed alongside
/// the `rwv.yaml` / `rwv.lock` change that motivated the verb.
///
/// See [`trigger-model.md`](../docs/repoweave/integration-ownership/trigger-model.md).
pub fn activate_intent(project: &str, cwd: &Path) -> anyhow::Result<()> {
    activate_intent_with_options(project, cwd, ActivateOptions::default())
}

/// Run intent-mode activation with explicit options. Used by tests that need
/// to drive the write path without running install hooks (the test
/// equivalent of `rwv add --no-install` if that existed).
pub fn activate_intent_with_options(
    project: &str,
    cwd: &Path,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
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
    cwd: &Path,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;

    // Guard: activate has no meaning inside a workweave. The project is fixed
    // at creation time (`rwv workweave <project> create <name>`), so there is
    // no project switch to make. Silently operating on primary from inside a
    // workweave (the status-quo before this fix) was surprising and unsafe —
    // it mutated primary's .rwv-active and weave-root symlinks as a side
    // effect of a command run from an unrelated workweave.
    if let WorkspaceLocation::Workweave { .. } = &ctx.location {
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

    // 2. Collect the owner-scoped surfacing set from all enabled
    //    integrations. The union of `generated_files()` and `managed_files()`
    //    is the complete set of root-relative paths that the framework
    //    symlinks for this project. The same union drives the owner-scoped
    //    removal predicate in step 3.
    let default_config = IntegrationConfig::default();
    let mut new_owned: BTreeSet<String> = BTreeSet::new();

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
            project: &project_name,
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

        for f in integration.generated_files(&int_ctx) {
            new_owned.insert(f);
        }
        for f in integration.managed_files(&int_ctx) {
            new_owned.insert(f);
        }
    }
    let new_generated: Vec<String> = new_owned.iter().cloned().collect();

    // 3. Remove old symlinks from a previous activation using the
    //    owner-scoped predicate: a root symlink is unlinked only if its
    //    name is in the **removal candidate set** AND `read_link` resolves
    //    to `projects/<some-project>/<that-file>`. Replaces the previous
    //    blanket "target has a `projects` component" check, which would
    //    sweep up unrelated symlinks (cf. rwv-c5h: the static-files
    //    framework concern).
    //
    //    The candidate set is the UNION of:
    //      - `new_owned` — the new project's integration outputs, and
    //      - the previously-active project's owned set (read .rwv-active,
    //        load its manifest, recompute) — without this, switching A→B
    //        leaves orphaned symlinks for integrations B doesn't enable
    //        (e.g. A had cargo + npm, B has only npm → Cargo.toml symlink
    //        would survive pointing at A).
    //    Each candidate's target is independently verified to resolve to
    //    `projects/<some-project>/<rel>` via the predicate.
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

    // 4. Create new symlinks at root pointing to project_dir files.
    //    Failures are collected as warnings so that partial symlink creation
    //    does not prevent .rwv-active from being written.
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
        let file_path = Path::new(file);
        let depth = file_path
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
            "activate: {error_count} integration activate-hook error(s); workspace may be partially activated"
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
pub fn deactivate(cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let root = ctx.primary_path();

    let owned = compute_active_owned_set(root)?;
    remove_activation_symlinks(root, &owned)?;

    let active_file = root.join(".rwv-active");
    if active_file.exists() {
        std::fs::remove_file(&active_file)?;
    }

    Ok(())
}

/// Compute the owner-scoped surfacing set for the currently-active project,
/// reading `.rwv-active` from `root`. Returns an empty set if no project is
/// active (in which case no symlinks are owned by rwv and nothing gets
/// removed). This is the deactivate-side analogue of step 2 in
/// [`activate_at`].
fn compute_active_owned_set(root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let mut owned: BTreeSet<String> = BTreeSet::new();
    let active = match crate::workspace::read_active_project(root) {
        Some(name) => name,
        None => return Ok(owned),
    };
    let project_dir = root.join("projects").join(active.as_str());
    let manifest_path = project_dir.join("rwv.yaml");
    if !manifest_path.exists() {
        return Ok(owned);
    }
    let manifest = Manifest::from_path(&manifest_path)?;
    let session = WorkspaceSession::new(root);
    let detection_cache = build_detection_cache(root, manifest.iter_entries());
    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let default_config = IntegrationConfig::default();

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
            project: &active,
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
        for f in integration.generated_files(&int_ctx) {
            owned.insert(f);
        }
        for f in integration.managed_files(&int_ctx) {
            owned.insert(f);
        }
    }
    Ok(owned)
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
}
