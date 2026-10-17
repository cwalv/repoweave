//! Activate and deactivate projects.
//!
//! Project **selection** — `rwv activate PROJECT` — makes the weave root
//! present a project by:
//! 1. Collecting the union of `generated_files()` and `managed_files()`
//!    from each enabled integration. This union is the **owner-scoped**
//!    surfacing set used both for symlink creation and for the removal
//!    predicate.
//! 2. Removing old symlinks (from a previous activation) using an
//!    **owner-scoped** predicate: a root symlink is unlinked only if its
//!    name is in the union AND `read_link` resolves to
//!    `projects/<some-project>/<that-file>`. This replaces the previous
//!    blanket "target contains a `projects` component" check, which
//!    swept up unrelated symlinks (e.g. workweave links into source-root
//!    paths under a `projects/` ancestor).
//! 3. Creating new symlinks at the workspace root pointing to the owned
//!    files in the project directory.
//! 4. Writing `.rwv-active` — **at a primary root only**. A workweave root's
//!    project is structural, fixed by its `.rwv-workweave` marker at
//!    creation; the two files are mutually exclusive. Steps 1–3 are
//!    weave-agnostic and are the whole of what `activate_workweave` does;
//!    step 4 sits outside them, in `activate_with_options`, where a
//!    `PrimaryIdentity` witness is what makes it expressible at all.
//!
//! **Regeneration** — intent mode — is a different operation on a different
//! scope: integrations author their managed/generated files into the target
//! project's own directory and `.rwv-active` is left alone, so `--project`
//! acts on a project without switching to it. Steps 1–3 re-run only when the
//! target is already the project the weave root presents, whose owned-file
//! union the regeneration may have changed.
//!
//! `deactivate` removes the symlinks and `.rwv-active`.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;

use crate::cli::consent::{DriftConsent, RemoveUndeclaredLinksConsent};
use crate::integration::{
    Integration, IssueKind, OwnedPath, Severity, SurfacedCopy, SurfacedSource,
};
use crate::integration_runner::{
    build_detection_cache, disabled_integration_artifacts, enabled_integrations,
    run_activate_hooks, run_activations, run_checks, run_deactivations, run_verifications,
    IntegrationContextBase,
};
use crate::integrations::builtin_integrations;
use crate::manifest::{IntegrationConfig, Manifest, ProjectName};
use crate::owned_state::{
    attested_owned_files, drifted_attested_owned_files, forget_owned_digest, ledger_path,
    reset_unreadable_ledger, stamp_owned_digest,
};
use crate::refusal::RefusalKind;
use crate::symlink::LinkTarget;
use crate::vcs::Vcs;
use crate::workspace::{
    observe_root, project_dir, project_rel_dir, scannable_registry_segments, strip_projects_prefix,
    Checkout, RootObservation, WorkspaceContext, WorkspaceSession, PROJECTS_DIR,
};

/// Which class of verb is driving activation.
#[derive(Debug, Clone, Copy)]
pub enum ActivationMode {
    /// The target project's integrations author their managed/generated
    /// content into that project's own directory. Regeneration is not
    /// selection: `.rwv-active` is never written, so `--project` targets a
    /// project without switching to it.
    Intent,
    /// Surfacing (symlink creation/repair) runs unconditionally; the
    /// integrations' `verify()` pass reports drift between on-disk
    /// content and what `activate()` would produce — but **never
    /// authors content**.
    Context,
    /// Only the hooks: the ecosystem state implied by current membership and
    /// the recorded pins is made real on disk. No authoring, no verify pass,
    /// and no claim on the weave root's shared names — the project acted on is
    /// the one the root already presents, so there is nothing to select.
    ///
    /// Carries the operator's [`DriftConsent`] because arriving at an attested
    /// generated file whose content rwv never accepted is a fork this mode
    /// alone reaches, and the choice out of it is the operator's.
    Materialize(Option<DriftConsent>),
}

/// What a surfacing call may do with the weave root's SHARED names — the
/// surfaced paths more than one project can produce (`Cargo.toml`, `go.work`,
/// `package.json`, whatever `static-files` declares). The root can show only
/// one project's, so which project's is a fact the root already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfacingMode {
    /// Project SELECTION: `project` becomes the project the root presents,
    /// and the shared names move with it. The file naming that project is
    /// written after surfacing, so this is the one call that cannot read it.
    Select,
    /// Repair of `project`'s surfacing without selecting it. Shared names
    /// stay with the project the root presents; for any other project only
    /// its per-project-named paths are surfaced.
    Repair,
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
/// Surfaces the existing on-disk artifacts and verifies them, **never
/// authoring**. Runs integration activate hooks (`npm install`, `uv sync`,
/// etc.) by default. See [`activate_with_options`] to suppress them.
pub fn activate(project: &str, ctx: &WorkspaceContext) -> anyhow::Result<()> {
    activate_with_options(project, ctx, ActivateOptions::default())
}

/// Regenerate `project`'s integration content in **intent mode**: it is
/// (re)authored into `projects/<project>/`, for the operator to commit
/// alongside the `rwv.toml` / `rwv.lock` change that motivated the verb.
///
/// This does **not** select `project`: `.rwv-active` is left untouched, and
/// the root's surfacing is refreshed only when `project` is already the
/// selected one. That is what makes `--project` a one-shot target rather
/// than a project switch.
pub fn activate_intent(project: &str, ctx: &WorkspaceContext) -> anyhow::Result<()> {
    activate_intent_with_options(project, ctx, ActivateOptions::default())
}

/// Run **intent-mode** activation bound to an explicit weave directory.
///
/// [`activate_intent`]'s weave-parameterized form: identical in every respect
/// except which weave root it targets, so `rwv doctor --fix` performs the same
/// repair inside a workweave that it performs at primary. Callers pass the
/// weave the detector scanned (`ctx.active_path()`), which is the primary root
/// at primary and the workweave directory inside a workweave — the isolation
/// contract pinned by `doctor_workweave_content_fix_isolation_test`.
///
/// **Install hooks run**, unlike the workweave-flavored
/// [`activate_workweave_intent`]. A `generated_files()` lockfile has no other
/// author — only `cargo generate-lockfile` / `npm install` / `uv sync`
/// produce one, so a hook-suppressed `--fix` reports the lock as regenerable
/// and then leaves it missing. Hooks only fire when doctor already found
/// safe-to-fix drift, so a clean weave still pays nothing.
pub fn activate_intent_at(project: &str, weave_dir: &Path) -> anyhow::Result<()> {
    activate_at(
        weave_dir,
        project,
        ActivateOptions::default(),
        ActivationMode::Intent,
    )
}

/// Run intent-mode activation with explicit options. Used by tests that need
/// to drive the write path without running install hooks (the test
/// equivalent of `rwv add --no-materialize` if that existed).
pub fn activate_intent_with_options(
    project: &str,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    activate_at(ctx.primary_path(), project, opts, ActivationMode::Intent)
}

/// Run `rwv materialize`: the integration hooks, for the project this checkout
/// already presents, with no claim on selection state.
///
/// Activation conflates two operations. SELECTION decides which project the
/// weave root presents — `.rwv-active`, the root's shared names — and only a
/// primary can express it. MATERIALIZATION makes the ecosystem state implied by
/// current membership and the recorded pins real on disk, and is meaningful in
/// any checkout whose project identity is already fixed: a workweave always,
/// a primary for the project it currently presents. This verb is the second
/// half alone, which is why it takes no project argument — a project name here
/// would be a selection, and the one operation this verb does not perform is
/// selection.
///
/// The refusal and the target come from one read of the root, so the case that
/// refuses is exactly the case with no project to name.
pub fn materialize(
    ctx: &WorkspaceContext,
    consent: Option<DriftConsent>,
    undeclared: Option<RemoveUndeclaredLinksConsent>,
) -> anyhow::Result<()> {
    let root = ctx.active_path();
    crate::op_state::check_no_op_in_progress(&[root]).context(
        "materialize does not start while an operation is in flight in this \
         workspace — it would regenerate from inputs that operation is still \
         rewriting. Wait for the operation to finish, or take one of the exits \
         it names",
    )?;

    let Some(project) = observe_root(root)
        .as_ref()
        .and_then(RootObservation::presented_project)
        .cloned()
    else {
        crate::refuse!(
            RefusalKind::NoActiveProject,
            "nothing is materialized at {}: no project is active here. \
             Run `rwv activate <name>` to select one first.",
            root.display()
        );
    };

    if undeclared.is_some() {
        remove_undeclared_links(root, &project)?;
    }

    activate_at(
        root,
        project.as_str(),
        ActivateOptions::default(),
        ActivationMode::Materialize(consent),
    )
}

/// Remove the weave-root links at names `project` no longer declares, naming
/// each as it goes.
///
/// Runs before the activation below rather than inside it: surfacing recreates
/// what the project DOES declare, and a removal that ran afterwards would be
/// deciding about a tree the same command had just rewritten. Announced per
/// link because an operator who reached this flag from `rwv doctor` should see
/// the same list act that they were shown, and one who did not still gets one.
fn remove_undeclared_links(root: &Path, project: &ProjectName) -> anyhow::Result<()> {
    let manifest_path = project_dir(root, project.as_str()).join(Manifest::FILE_NAME);
    let manifest = Manifest::from_path(&manifest_path)?;
    let declared = owned_paths(root, project, &manifest);
    let links = undeclared_project_links(root, project, &manifest, &declared);
    if links.is_empty() {
        println!("core: no weave-root links at undeclared names; nothing to remove");
        return Ok(());
    }
    for link in &links {
        println!(
            "[removed] core: `{}` -> `{}` (a link at a name `{}` no longer declares; \
             the file it pointed at is untouched)",
            link.name(),
            crate::path_spelling::weave_relative(link.target()),
            project
        );
    }
    unsurface_undeclared(&links)
}

/// Remove what a disabled integration authored: the state disablement implies
/// is absence, and this is the verb that makes an implied state real.
///
/// Runs before [`settle_arrived_drift`], and the order is the answer to a
/// question neither rule settles alone. Drift asks which of two futures an
/// attested file should have — accept these bytes, or discard them and generate
/// again. A file whose author is disabled has neither future, so both consents
/// would misdescribe what happens to it, and `--adopt-drifted` would read as
/// "record this content" while the file is deleted. Stripping first means the
/// fork is never reached for a file that is going away, and the ledger entry
/// goes with the file, since an attestation of something absent is stale by
/// construction.
///
/// Each file removal is announced. Doctor's finding is the loss list an
/// operator reads before choosing this verb, but an operator who never ran
/// doctor still gets one — from the operation itself, as it acts.
///
/// A weave-root link is a separate object from the file it surfaces, owned by
/// rwv regardless of who authored that file, so [`strip_disabled_links`]
/// clears every link a disabled integration declares unconditionally — the
/// file-removal loop above stays scoped to `owned_paths_on_disk`, which
/// answers only for the file.
fn strip_disabled_integrations(
    root: &Path,
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    ctx_base: &crate::integration_runner::IntegrationContextBase,
) -> anyhow::Result<()> {
    let found = disabled_integration_artifacts(integrations, manifest, ctx_base);
    let output_dir = ctx_base.output_dir.as_path();
    let default_config = IntegrationConfig::default();

    for artifacts in &found {
        let Some(integration) = integrations
            .iter()
            .find(|i| i.name() == artifacts.integration)
        else {
            continue;
        };
        // The integration's own cleanup shape, and ONLY for the class that
        // needs it: taking rwv's region out of a file the operator co-owns
        // needs the format knowledge that lives in the integration. Whole
        // files are removed below instead, because `deactivate` removes the
        // ones it declares without asking whether rwv wrote them — harmless
        // where it was already wired (the checkout is being deleted around
        // it), and a deletion of operator content here.
        if artifacts
            .paths
            .iter()
            .any(|path| matches!(path, OwnedPath::MarkedRegion(_)))
        {
            integration.deactivate(output_dir).with_context(|| {
                format!(
                    "{}: stripping the content of a disabled integration",
                    artifacts.integration
                )
            })?;
        }

        for path in &artifacts.paths {
            let OwnedPath::WholeFile(name) = path else {
                continue;
            };
            let file = output_dir.join(name);
            if file.is_file() {
                std::fs::remove_file(&file)
                    .with_context(|| format!("removing {}", file.display()))?;
            }
            forget_owned_digest(output_dir, name)?;
            prune_emptied_parent(output_dir, &file);
        }

        let names: Vec<String> = artifacts
            .paths
            .iter()
            .map(|p| p.name().to_string())
            .collect();
        unsurface_names(root, &names)?;
        let config = manifest
            .integrations
            .get(artifacts.integration.as_str())
            .unwrap_or(&default_config);
        let ctx = ctx_base.build_context(config, manifest);
        let remaining = integration.owned_paths_on_disk(&ctx);
        eprintln!(
            "[stripped] {}: disabled, removed {}",
            artifacts.integration,
            names.join(", ")
        );
        if !remaining.is_empty() {
            anyhow::bail!(
                "{}: strip left {} behind; the finding that named this verb would \
                 fire again",
                artifacts.integration,
                remaining
                    .iter()
                    .map(|p| p.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    strip_disabled_links(root, manifest, ctx_base)
}

/// Unsurface every weave-root link a disabled integration declares, whether
/// or not [`Integration::owned_paths_on_disk`] still evidences its authorship
/// of what the link points at.
///
/// The predicate the file-removal loop above applies — content on disk still
/// matches what rwv would author — exists to protect an operator's edit from
/// being deleted; a link carries no content of its own to protect, and taking
/// it down destroys nothing the operator wrote. So this reuses
/// [`disabled_integration_declarations`] — the same full
/// `generated_files() ∪ managed_files()` union [`undeclared_project_links`]
/// holds out of the general scan — rather than the authorship-filtered
/// subset above: a name a disabled integration ever declared is a name its
/// link may still be stranded at. [`unsurface_names`] is a no-op for a name
/// with no matching symlink, so re-offering names the loop above already
/// cleared costs nothing.
///
/// Silent: an operator reading this output must never see the path of a file
/// they authored, and for a `SurfacedSource::WrittenAtSource` declaration the
/// link's own name and the file's are the same string.
fn strip_disabled_links(
    root: &Path,
    manifest: &Manifest,
    ctx_base: &crate::integration_runner::IntegrationContextBase,
) -> anyhow::Result<()> {
    let names: Vec<String> = disabled_integration_declarations(root, ctx_base.project, manifest)
        .into_iter()
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    unsurface_names(root, &names)
}

/// Remove the directory a removed file lived in, if that removal emptied it and
/// it is not the project directory itself.
///
/// Best effort by construction: `remove_dir` refuses a non-empty directory, and
/// a non-empty one is the case to leave alone — anything else parked there is
/// the operator's.
fn prune_emptied_parent(output_dir: &Path, removed: &Path) {
    if let Some(parent) = removed.parent() {
        if parent != output_dir {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// Drop attestations for files no enabled integration produces.
///
/// An attestation is a claim that rwv generated a file from recorded inputs.
/// When nothing enabled here declares that file any more — the last member of
/// its ecosystem left, or its integration was just stripped — rwv produces it
/// no longer, and a claim about a derivation it will never redo is a standing
/// finding no verb could ever clear. The file itself is not touched: whether an
/// orphaned generated file should survive its producer is a different question
/// from whether rwv still vouches for it.
fn forget_unproduced_attestations(
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    ctx_base: &crate::integration_runner::IntegrationContextBase,
) -> anyhow::Result<()> {
    let output_dir = ctx_base.output_dir.as_path();
    let attested = attested_owned_files(output_dir);
    if attested.is_empty() {
        return Ok(());
    }
    let default_config = IntegrationConfig::default();
    // Fully-owned declaration, not [`SurfacedSource`]: an attestation follows
    // whether rwv vouches for the bytes, and both a lock a hook writes and a
    // CSV `activate()` writes are vouched for while differing on where the
    // write lands.
    let declared_fully_owned: BTreeSet<String> =
        enabled_integrations(integrations, manifest, &default_config)
            .flat_map(|(integration, config)| {
                let ctx = ctx_base.build_context(config, manifest);
                integration.generated_files(&ctx)
            })
            .map(|f| f.into_parts().0)
            .collect();

    for name in attested
        .iter()
        .filter(|name| !declared_fully_owned.contains(*name))
    {
        forget_owned_digest(output_dir, name)?;
        eprintln!(
            "[forgot] core: nothing enabled here generates {name}; rwv no longer \
             vouches for it"
        );
    }
    Ok(())
}

/// Settle attested generated files in `output_dir` whose content rwv never
/// accepted, before anything downstream reads or rewrites them.
///
/// Runs ahead of the hooks because a hook that regenerates re-stamps whatever
/// it produced, which turns arriving at drift into silently adopting it. The
/// two exits destroy opposite things — regenerating discards content the
/// operator may have written deliberately, adopting attests content that may be
/// an accident — so with no consent this refuses and names both.
///
/// Regenerating is a removal here rather than a call into any integration:
/// a ledger entry exists only where a generator's output was accepted, every
/// such generator runs in the activate hook this function precedes, and each of
/// those files is fully owned — whole-write and whole-delete safe by the
/// declaration that put it in the ledger.
fn settle_arrived_drift(output_dir: &Path, consent: Option<DriftConsent>) -> anyhow::Result<()> {
    let drifted = drifted_attested_owned_files(output_dir);
    if drifted.is_empty() {
        return Ok(());
    }

    match consent {
        None => {
            let listed = drifted
                .iter()
                .map(|file| {
                    format!(
                        "\n  {}",
                        crate::path_spelling::operator_path(&output_dir.join(&file.name))
                    )
                })
                .collect::<String>();
            crate::refuse!(
                RefusalKind::UnacceptedGeneratedContent,
                "materialize stopped: content rwv never accepted is on disk for \
                 {} generated file(s) it attests:{listed}\n\
                 Re-run with `--adopt-drifted` to record the current content as \
                 the accepted generation, or `--regenerate-drifted` to discard it \
                 and regenerate from the current inputs.",
                drifted.len()
            );
        }
        Some(DriftConsent::Adopt(_)) => {
            for file in &drifted {
                stamp_owned_digest(output_dir, &file.name, &file.content).with_context(|| {
                    format!(
                        "adopting the current content of {}",
                        output_dir.join(&file.name).display()
                    )
                })?;
            }
        }
        Some(DriftConsent::Regenerate(_)) => {
            for file in &drifted {
                let path = output_dir.join(&file.name);
                std::fs::remove_file(&path)
                    .with_context(|| format!("discarding drifted {}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// Options for [`activate_with_options`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ActivateOptions {
    /// When true, skip integration activate hooks (install commands like
    /// `npm install`). Used by `rwv activate --no-materialize` for fast
    /// context-switches.
    pub no_materialize: bool,
}

/// Run activate with options. Public so the CLI can pass `--no-materialize`.
///
/// The witness lookup is the workweave guard. Activate has no meaning inside
/// a workweave — the project is fixed at creation time (`rwv workweave
/// <project> create <name>`), so there is no switch to make, and silently
/// retargeting primary from inside one (the status quo before this fix)
/// mutated primary's `.rwv-active` and weave-root symlinks as a side effect
/// of a command run from an unrelated workweave. A workweave has no witness,
/// so the two questions are one lookup.
pub fn activate_with_options(
    project: &str,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    // Named for the act, not for a verb: `init` and `fetch` activate in-process
    // too, and a refusal naming `activate` would name a verb its reader did not
    // run.
    crate::op_state::check_no_op_in_progress(&[ctx.active_path()]).context(
        "surfacing a project and running its install hooks does not start while \
         an operation is in flight in this workspace — they would run over repos \
         that operation is still moving. Wait for the operation to finish, or \
         take one of the exits it names",
    )?;

    let primary = match ctx.primary_identity() {
        Some(identity) => identity,
        None => crate::refuse!(
            RefusalKind::WrongCheckoutKind,
            "rwv activate has no effect in a workweave (project is fixed at creation). \
             cd to primary ({}) and rerun.",
            crate::path_spelling::operator_path(ctx.primary_path())
        ),
    };

    activate_at(ctx.primary_path(), project, opts, ActivationMode::Context)?;

    // Project SELECTION, after activation rather than inside it. An activate
    // hook that errored bails above, so a partially-activated workspace
    // still does not record success.
    primary.select_project(&ProjectName::new(project)?)
}

/// Run every enabled integration's `deactivate` against `project_dir`.
///
/// The caller is destroying the checkout `project_dir` belongs to. Nothing here
/// is scoped to a weave root or to selection: a project that is merely not the
/// selected one keeps its ecosystem files, which are its own repo's content.
pub fn strip_project_regions(
    project_dir: &Path,
    manifest: &Manifest,
) -> Vec<crate::integration::Issue> {
    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    run_deactivations(&integrations, manifest, project_dir)
}

/// Shared activation logic.
///
/// `mode`: which class of verb is driving activation (see [`ActivationMode`]).
/// In `Intent` mode the integrations' `activate()` is called (regenerate and
/// commit). In `Context` mode the integrations' `verify()` is called instead
/// (surface + verify, never author).
fn activate_at(
    root: &Path,
    project: &str,
    opts: ActivateOptions,
    mode: ActivationMode,
) -> anyhow::Result<()> {
    activate_in_phases(root, project, opts, mode).map_err(ActivationFailure::into_error)
}

/// How far an activation got before it failed, which is what decides whether
/// its caller can still undo it.
#[derive(Debug)]
pub(crate) enum ActivationFailure {
    /// Failed before any install hook ran, in the content step or in
    /// surfacing. Surfacing is all-or-nothing, so what is left to undo is
    /// whatever the content step wrote into the project directory.
    BeforeHooks(anyhow::Error),
    /// Failed at the install hooks, after the content and surfacing had
    /// landed. What a hook does to the disk — an installed dependency tree, a
    /// rewritten lock — has no inverse.
    AtHooks(anyhow::Error),
}

impl ActivationFailure {
    pub(crate) fn into_error(self) -> anyhow::Error {
        match self {
            ActivationFailure::BeforeHooks(e) | ActivationFailure::AtHooks(e) => e,
        }
    }
}

/// Intent-mode activation for the checkout `ctx` resolved —
/// [`activate_workweave_intent`] inside a workweave, [`activate_intent`] at
/// primary — reporting how far it got when it fails.
pub(crate) fn activate_intent_for(
    project: &str,
    ctx: &WorkspaceContext,
) -> Result<(), ActivationFailure> {
    match &ctx.checkout {
        Checkout::Workweave { dir, .. } => workweave_intent(project, dir),
        Checkout::Primary { .. } => activate_in_phases(
            ctx.primary_path(),
            project,
            ActivateOptions::default(),
            ActivationMode::Intent,
        ),
    }
}

fn activate_in_phases(
    root: &Path,
    project: &str,
    opts: ActivateOptions,
    mode: ActivationMode,
) -> Result<(), ActivationFailure> {
    let project_name =
        ProjectName::new(project).map_err(|e| ActivationFailure::BeforeHooks(e.into()))?;
    let manifest_path = project_dir(root, project).join(Manifest::FILE_NAME);
    let manifest = Manifest::from_path(&manifest_path).map_err(ActivationFailure::BeforeHooks)?;

    // Discover repos on disk and project paths (needed by IntegrationContext).
    let session = WorkspaceSession::new(root);

    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();

    let detection_cache = build_detection_cache(&integrations, root, manifest.iter_entries());
    let ctx_base =
        session.context_base(&project_name, &detection_cache, manifest.workweave.as_ref());
    let project_vcs = crate::vcs::project_vcs();

    let Some(noticed) = author_and_surface(
        project_vcs.as_ref(),
        root,
        &project_name,
        &manifest,
        &integrations,
        &ctx_base,
        mode,
    )
    .map_err(ActivationFailure::BeforeHooks)?
    else {
        return Ok(());
    };
    run_hooks(
        project_vcs.as_ref(),
        &integrations,
        &manifest,
        &ctx_base,
        opts,
        mode,
        &noticed,
    )
    .map_err(ActivationFailure::AtHooks)
}

/// The integration content step for `mode`, then the weave root's surfacing,
/// returning what surfacing reported. `None` when the root presents a different
/// project, which leaves it, and the hooks that act through it, untouched.
fn author_and_surface(
    project_vcs: &dyn Vcs,
    root: &Path,
    project_name: &ProjectName,
    manifest: &Manifest,
    integrations: &[&dyn Integration],
    ctx_base: &IntegrationContextBase,
    mode: ActivationMode,
) -> anyhow::Result<Option<Vec<String>>> {
    match mode {
        ActivationMode::Intent => {
            let issues = run_activations(integrations, manifest, ctx_base);
            report_and_check_activation_issues(&issues)?;
        }
        ActivationMode::Context => {
            // Two passes:
            //   - run_checks: environment/config preconditions (cargo not on
            //     PATH, declared static file missing, etc.). Same call as
            //     `rwv doctor`. Surfaces as warnings; does not bail.
            //   - run_verifications: drift between intent (rwv.toml/.lock)
            //     and on-disk managed/generated content. Empty by default;
            //     ports override `verify()` to opt in.
            // Context verbs **never author content**, so even Severity::Error
            // findings are surfaced as warnings and the activation proceeds —
            // the recovery hatch is `rwv doctor --fix`. Bailing here would
            // defeat the spec's unqualified "activate never authors" contract.
            let check_issues = run_checks(integrations, manifest, ctx_base);
            for issue in &check_issues {
                let prefix = match issue.severity {
                    Severity::Warning => "warning",
                    Severity::Error => "warning",
                };
                eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
            }
            let verify_issues = run_verifications(integrations, manifest, ctx_base);
            for issue in &verify_issues {
                let prefix = match issue.severity {
                    Severity::Warning => "warning (drift)",
                    Severity::Error => "warning (drift)",
                };
                eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
            }
        }
        ActivationMode::Materialize(consent) => {
            strip_disabled_integrations(root, integrations, manifest, ctx_base)?;
            forget_unproduced_attestations(integrations, manifest, ctx_base)?;
            // Before the drift check, so a tool's write that missed its link is
            // judged in the project copy the check reads, as one that went
            // through the link always is.
            surface_with_notices(
                project_vcs,
                root,
                project_name,
                manifest,
                SurfacingMode::Repair,
            )?;
            settle_arrived_drift(&ctx_base.output_dir, consent)?;
        }
    }

    // Everything below acts on the weave ROOT. Which project the root
    // presents is `presented_project` — the single tier of the resolution
    // chain, answering from `.rwv-workweave` in a workweave root and from
    // `.rwv-active` at primary. Reading the pointer directly here would be
    // wrong in a workweave, whose root carries no pointer: every intent verb
    // (`add`, `remove`, `update`) and `doctor --fix` run inside one would
    // take the early return below and silently skip surfacing.
    //
    // Intent mode regenerates a project's content without choosing it, so it
    // touches the root only when the target is already the project the root
    // presents — whose owned-file union the regeneration it just ran may have
    // changed.
    if matches!(mode, ActivationMode::Intent) {
        let observed = observe_root(root);
        if observed
            .as_ref()
            .and_then(RootObservation::presented_project)
            != Some(project_name)
        {
            return Ok(None);
        }
    }

    // Surface the owner-scoped symlink set: compute the
    //    `generated_files() ∪ managed_files()` union, remove stale
    //    owner-scoped symlinks, and (re)create the symlinks at `root`
    //    pointing into `projects/<project>/`. It is the surfacing path that
    //    workweave-create also runs, and is re-runnable on its own — it
    //    does NOT write `.rwv-active` (project SELECTION) and does NOT
    //    author integration content.
    //    Context mode IS the selection verb, so it may move the root's shared
    //    names; intent and materialize modes reached this line only because the
    //    root already presents `project_name`, which is the same permission by
    //    the other route.
    let surfacing_mode = match mode {
        ActivationMode::Context => SurfacingMode::Select,
        ActivationMode::Intent | ActivationMode::Materialize(_) => SurfacingMode::Repair,
    };
    let notices = surface_with_notices(project_vcs, root, project_name, manifest, surfacing_mode)?;
    for notice in &notices {
        eprintln!("[warning] core: surfacing: {notice}");
    }
    Ok(Some(notices))
}

/// Run integration activate hooks (install commands). They operate on the
/// now-in-place symlinks at the workspace root (e.g., `npm install` reads the
/// symlinked package.json). Suppressed by `--no-materialize` for fast
/// context-switches; the user can run install commands directly when they need
/// them.
fn run_hooks(
    project_vcs: &dyn Vcs,
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    ctx_base: &IntegrationContextBase,
    opts: ActivateOptions,
    mode: ActivationMode,
    noticed: &[String],
) -> anyhow::Result<()> {
    if !opts.no_materialize && !withhold_hooks_over_unsettled_drift(&ctx_base.output_dir) {
        refuse_hooks_over_malformed_settings(integrations, manifest, ctx_base)?;
        let hook_issues = run_activate_hooks(integrations, manifest, ctx_base);
        report_and_check_activate_hook_issues(&hook_issues)?;
        // A tool the hooks ran may have written past its link — a first
        // generation by a renaming writer replaces the link that dangled.
        let after_hooks = match mode {
            ActivationMode::Intent | ActivationMode::Materialize(_) => surface_with_notices(
                project_vcs,
                ctx_base.workspace_root,
                ctx_base.project,
                manifest,
                SurfacingMode::Repair,
            )?,
            ActivationMode::Context => {
                root_write_notices(ctx_base.workspace_root, ctx_base.project, manifest)
            }
        };
        for notice in after_hooks
            .iter()
            .filter(|notice| !noticed.contains(notice))
        {
            eprintln!("[warning] core: surfacing: {notice}");
        }
    }

    if matches!(mode, ActivationMode::Materialize(_)) {
        reset_ledger_no_generator_rebuilt(&ctx_base.output_dir)?;
    }

    Ok(())
}

/// Make `unreadable-owned-state` clearable in a project whose integrations
/// generate nothing, by emptying a ledger the run just failed to rebuild.
///
/// The advised repair for that finding is this verb, and the rebuild it names
/// is a side effect of stamping: a generator runs, and the stamp rewrites the
/// whole ledger on its way past. Where nothing here generates a fully-owned
/// file, no stamp happens, and every earlier step of materialize is gated on
/// reading the ledger it would have to write. Nothing moves, and the operator
/// re-runs the one verb the finding names against a finding that never clears.
///
/// Last, so the two populations do not collide. A generator that ran has
/// already written a record of real derivations, which this must not overwrite
/// with an empty one, and a hook that FAILED returns above this line — leaving
/// the fault standing, which is correct for a run that rebuilt nothing.
fn reset_ledger_no_generator_rebuilt(output_dir: &Path) -> anyhow::Result<()> {
    let Some(error) = reset_unreadable_ledger(output_dir)? else {
        return Ok(());
    };
    eprintln!(
        "[reset] core: {} did not parse ({error}) and nothing here regenerated \
         it, so it is now an empty record — rwv attests no generated file for \
         this project until one is generated again",
        crate::path_spelling::operator_path(&ledger_path(output_dir))
    );
    Ok(())
}

/// Whether the hooks must not run, because arrived drift is still unsettled.
///
/// A hook re-runs its generator and re-attests what it produced, so over an
/// attested file rwv never accepted it takes one of the two drift exits with
/// nobody's consent — and which exit is not even determined, since the
/// ecosystem tool decides whether it rewrites the bytes or keeps them. That is
/// the laundering the consents exist to prevent, so the answer here is the same
/// for every verb that reaches this line, and this asks no question about which
/// one did.
///
/// [`settle_arrived_drift`] leaves nothing for this to find in the one mode
/// that carries a consent: without one it bails, and with one it has already
/// stamped or discarded every drifted file by the time surfacing is done.
///
/// Withholding rather than refusing: the intent verbs write the manifest before
/// they regenerate, so a bail here would exit non-zero over a change that
/// already landed. The manifest change stands, the operator is told what did
/// not happen, and the two consents that unblock it are named.
///
/// The unit is the workspace, not the integration whose file drifted: one
/// drifted lock withholds every integration's hooks. Narrowing it to the
/// declaring integration would let the others re-run over a membership the
/// drifted one does not share, which is a partially regenerated workspace no
/// verb afterwards can name. The cost is a `npm install` withheld over a
/// `Cargo.lock`, and it is the accepted one.
fn withhold_hooks_over_unsettled_drift(output_dir: &Path) -> bool {
    let drifted = drifted_attested_owned_files(output_dir);
    if drifted.is_empty() {
        return false;
    }
    let listed = drifted
        .iter()
        .map(|file| {
            format!(
                "\n  {}",
                crate::path_spelling::operator_path(&output_dir.join(&file.name))
            )
        })
        .collect::<String>();
    eprintln!(
        "[withheld] core: the install hooks were not run. They re-run each \
         generator and record what it produces as accepted, which would settle \
         the content rwv never accepted on disk for {} generated file(s) it \
         attests, without the consent that says which way:{listed}\n\
         Everything else this command does has already happened, so any \
         manifest or managed-file change it made is landed while the file(s) \
         above have NOT been re-derived from it.\n\
         Choose it: `rwv materialize --adopt-drifted` records the current \
         content as the accepted generation, `rwv materialize \
         --regenerate-drifted` discards it and regenerates from the current \
         inputs. Then re-run this command.",
        drifted.len()
    );
    true
}

/// Refuse the install hooks while an `[integrations.<name>]` block does not
/// deserialize into the settings type its integration declares.
///
/// A hook that reads its own settings has nothing to act on, and reading them
/// raw it bails — which the runner captures as `IssueKind::IntegrationFailed`,
/// whose advised repair is a regeneration that would have to read the same
/// block it just failed to read. Refusing here is what keeps the cause attached
/// to its remedy: only an edit to `rwv.toml` makes the block readable.
///
/// Placed at the hooks rather than in an [`ActivationMode`] arm: `Intent` gates
/// on `activate()`'s own findings and `Context` reports check and verify
/// findings without gating on either, so a guard written per mode is a guard
/// each new mode must remember. This is the one line every mode reaches the
/// hooks through.
///
/// The population is what `check()` reports. An integration whose `check()`
/// does not deserialize its settings contributes nothing here, and its hook
/// keeps only whatever guard it wrote for itself.
fn refuse_hooks_over_malformed_settings(
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    ctx_base: &IntegrationContextBase,
) -> anyhow::Result<()> {
    let malformed: Vec<_> = run_checks(integrations, manifest, ctx_base)
        .into_iter()
        .filter(|issue| issue.kind == IssueKind::MalformedSettings)
        .collect();
    if malformed.is_empty() {
        return Ok(());
    }
    let listed = malformed
        .iter()
        .map(|issue| format!("\n  {}: {}", issue.integration, issue.message))
        .collect::<String>();
    anyhow::bail!(
        "activate: the install hooks were not run. {} `[integrations.<name>]` \
         block(s) in rwv.toml yielded no settings, and a hook that reads its own \
         has nothing to act on:{listed}\n\
         Edit rwv.toml at the field each line names, then re-run.",
        malformed.len()
    );
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
/// resolved source-root path happens to live under a `projects/` ancestor).
/// A directory a removal empties is pruned; one that was already empty is not
/// the sweep's to judge, since git tracks no empty directory and an operator's
/// scratch directory looks exactly like one.
///
/// Each removal is appended to `removed` as it happens, so a caller holds the
/// record of a sweep that failed partway.
fn remove_activation_symlinks(
    root: &Path,
    sites: &SurfacingSites,
    owned_files: &BTreeSet<String>,
    removed: &mut Vec<RemovedLink>,
) -> anyhow::Result<()> {
    let skip = sweep_descent_skip(root);
    remove_activation_symlinks_in(root, root, sites, owned_files, &skip, removed)
}

/// A weave-root link a sweep unlinked, and the target it read.
struct RemovedLink {
    link: std::path::PathBuf,
    target: std::path::PathBuf,
}

/// The weave-root directories a symlink sweep must not descend into: every
/// registry segment a project manifest under `root` names, plus the built-ins,
/// plus `projects/` itself. Computed once at each sweep's entry point —
/// recursing into a candidate skip name would re-read every project manifest
/// per directory entry.
///
/// Held as paths under `root`, not as names: those segments mean a registry
/// or the project tree only at the top level, and a directory deeper down that
/// shares the name is ordinary content.
fn sweep_descent_skip(root: &Path) -> BTreeSet<std::path::PathBuf> {
    let mut skip: BTreeSet<std::path::PathBuf> = scannable_registry_segments(root)
        .into_iter()
        .map(|segment| root.join(segment))
        .collect();
    skip.insert(root.join(PROJECTS_DIR));
    skip
}

fn sweep_skips(dir: &Path, skip: &BTreeSet<std::path::PathBuf>) -> bool {
    skip.contains(dir)
        || dir
            .file_name()
            .is_some_and(|name| name == crate::git::GIT_DIR_ENTRY_NAME)
}

/// Which of `names` the weave root currently surfaces out of a project, in
/// `names` order. Read-only.
///
/// Owner-scoped by the same predicate the removal path uses, so a root entry
/// that is a real file, a dangling name, or a link pointing somewhere else is
/// not counted as surfacing.
pub fn surfaced_names(root: &Path, names: &[String]) -> Vec<String> {
    let sites = SurfacingSites::of(root);
    names
        .iter()
        .filter(|name| {
            std::fs::read_link(root.join(name))
                .is_ok_and(|target| target_resolves_to_projects(&sites, Path::new(name), &target))
        })
        .cloned()
        .collect()
}

/// Unlink the weave-root symlinks surfacing `names`, and prune directories the
/// removal empties.
///
/// Owner-scoped: this is the activation-symlink cleanup pointed at one name set
/// rather than at a whole activation's, and it removes nothing
/// [`surfaced_names`] would not have counted.
pub fn unsurface_names(root: &Path, names: &[String]) -> anyhow::Result<()> {
    remove_activation_symlinks(
        root,
        &SurfacingSites::of(root),
        &names.iter().cloned().collect(),
        &mut Vec::new(),
    )
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
///
/// `<project>` is not a fixed number of components — a project name may
/// itself carry `/` — so the split is read off the two known lengths
/// instead of assumed: whatever sits under `projects/` beyond the trailing
/// `rel_from_root` components is the project's own directory, however many
/// components that takes.
///
/// At a workweave root the absolute target a primary-sourced link reads,
/// `<primary>/projects/<project>/<rel>`, is the same shape rooted at the
/// root's own primary — see [`SurfacingSites`] for what keeps it narrow.
fn target_resolves_to_projects(
    sites: &SurfacingSites,
    rel_from_root: &Path,
    target: &Path,
) -> bool {
    surfacing_owner_dir(sites, rel_from_root, target).is_some()
}

/// The project directory `target` surfaces `rel_from_root` out of, when it has
/// the shape [`target_resolves_to_projects`] accepts — the same walk, returning
/// which project it landed in rather than only that it landed.
///
/// The two questions share one implementation deliberately. The owner-scoped
/// removal path already trusts this shape to authorize an unlink, so a second
/// reader asking "and whose is it" must not be able to accept a shape the first
/// one rejects, or the sweep that reads it would reach links the audited
/// predicate says are none of rwv's business.
///
/// Returned as the directory segment rather than a `ProjectName`, because the
/// caller compares it against a project it already holds. Parsing here would
/// let a name this repo's validator happens to reject narrow a predicate whose
/// acceptance set is load-bearing elsewhere.
fn surfacing_owner_dir(
    sites: &SurfacingSites,
    rel_from_root: &Path,
    target: &Path,
) -> Option<String> {
    if target.is_absolute() {
        return sites.primary_owner_dir(rel_from_root, target);
    }
    let mut comps = target.components().peekable();
    // Skip any leading parent-dir components (`../../...` for nested links).
    while let Some(c) = comps.peek() {
        if c.as_os_str() == ".." {
            comps.next();
        } else {
            break;
        }
    }
    let sited: std::path::PathBuf = comps.collect();
    owner_dir_under_projects(rel_from_root, strip_projects_prefix(&sited)?)
}

/// The project directory `under_projects` (a target with its `projects/`
/// prefix taken off) surfaces `rel_from_root` out of: whatever precedes a
/// tail equal to `rel_from_root`.
fn owner_dir_under_projects(rel_from_root: &Path, under_projects: &Path) -> Option<String> {
    let under_components: Vec<_> = under_projects.components().collect();
    let rel_components: Vec<_> = rel_from_root.components().collect();
    if under_components.len() <= rel_components.len() {
        return None;
    }
    let split = under_components.len() - rel_components.len();
    if under_components[split..] != rel_components[..] {
        return None;
    }
    let project: std::path::PathBuf = under_components[..split].iter().collect();
    project.to_str().map(str::to_string)
}

fn remove_activation_symlinks_in(
    dir: &Path,
    root: &Path,
    sites: &SurfacingSites,
    owned_files: &BTreeSet<String>,
    skip: &BTreeSet<std::path::PathBuf>,
    removed: &mut Vec<RemovedLink>,
) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    let removed_before = removed.len();

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match path.symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                // Owner-scoped predicate: unlink only when both (a) the
                // symlink's name is in the active integrations' union, AND
                // (b) its target resolves to
                // `projects/<some-project>/<that-file>`. A symlink whose
                // name we don't claim, or whose target points elsewhere
                // (e.g. workweave.link → source-root path), is preserved.
                let rel_from_root = match path.strip_prefix(root) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                // The owned set is keyed by the integrations' declared
                // names, whose separators are `/` by contract; the walk
                // renders this path with the platform's, so the lookup
                // converts spelling at the boundary or a nested link is
                // never recognized as owned where the platform writes `\`.
                let rel_str = rel_from_root.to_string_lossy().replace('\\', "/");
                let in_owned_set = owned_files.contains(rel_str.as_str());
                let resolves_to_projects =
                    target_resolves_to_projects(sites, rel_from_root, &target);
                if in_owned_set && resolves_to_projects {
                    crate::symlink::remove(&path)?;
                    removed.push(RemovedLink { link: path, target });
                }
            }
        } else if meta.file_type().is_dir() && !sweep_skips(&path, skip) {
            remove_activation_symlinks_in(&path, root, sites, owned_files, skip, removed)?;
        }
    }

    if dir != root && removed.len() > removed_before {
        let _ = std::fs::remove_dir(dir);
    }

    Ok(())
}

/// Run activation in a workweave directory without calling resolve.
///
/// This is used by `create_workweave` after the workweave is fully set up.
/// Unlike `activate`, it does not call `WorkspaceContext::resolve_invocation` (which would
/// return the primary root via the `.rwv-workweave` marker). Instead it works
/// directly against the workweave directory.
///
/// Install hooks (`npm install`, `cargo fetch`, …) are
/// skipped at workweave creation: the workweave shares clones with
/// primary, so install state is typically inherited rather than
/// regenerated. `rwv materialize` is what runs them inside the workweave when
/// a refresh is actually wanted.
///
/// Context mode here buys the surfacing-and-verify half only. The project
/// SELECTION half — writing `.rwv-active` — is not skipped so much as
/// absent: it lives in [`activate_with_options`] behind a `PrimaryIdentity`,
/// and `workweave_dir` is a workweave root, whose project the
/// `.rwv-workweave` marker already names.
pub fn activate_workweave(project: &str, workweave_dir: &Path) -> anyhow::Result<()> {
    activate_at(
        workweave_dir,
        project,
        ActivateOptions {
            no_materialize: true,
        },
        ActivationMode::Context,
    )
}

/// Run **intent-mode** activation inside a workweave: content is authored
/// into the workweave's project directory, which writes through to the
/// project repo the workweave is a view onto.
///
/// Install hooks remain suppressed in the workweave (mirroring
/// [`activate_workweave`]).
///
/// Used by the intent verbs (`add`, `remove`, `update`) when they run inside
/// a workweave. `rwv doctor --fix` deliberately does NOT come through here —
/// it uses [`activate_intent_at`], which runs the hooks; see that function
/// for why a repair verb cannot suppress them.
pub fn activate_workweave_intent(project: &str, workweave_dir: &Path) -> anyhow::Result<()> {
    workweave_intent(project, workweave_dir).map_err(ActivationFailure::into_error)
}

fn workweave_intent(project: &str, workweave_dir: &Path) -> Result<(), ActivationFailure> {
    activate_in_phases(
        workweave_dir,
        project,
        ActivateOptions {
            no_materialize: true,
        },
        ActivationMode::Intent,
    )
}

/// One entry of the surfacing union: who declared the path, and whether its
/// link may stand over a file that does not exist yet.
struct SurfacedEntry {
    integration: String,
    source: SurfacedSource,
    copy: SurfacedCopy,
}

impl SurfacedEntry {
    /// Take in a second declaration of the same path. Written-through wins,
    /// since suppressing a link a tool needs strands its output; and the
    /// primary's copy wins, since a declarer asking for shared state is
    /// asking for the one copy every workweave sees.
    fn absorb(&mut self, source: SurfacedSource, copy: SurfacedCopy) {
        if source == SurfacedSource::WrittenThroughLink {
            self.source = source;
        }
        if copy == SurfacedCopy::Primary {
            self.copy = copy;
        }
    }
}

/// Whether the link for this declaration should not exist yet: its file has to
/// be written at the source, and nothing has written it.
///
/// The one predicate [`surface_symlinks`] and [`verify_surfacing`] share, so
/// what the creator declines to make is exactly what the check declines to
/// expect. Splitting it is how the two came to disagree at the same root.
fn link_waits_for_its_source(entry: &SurfacedEntry, source: &Path) -> bool {
    entry.source == SurfacedSource::WrittenAtSource && !source.exists()
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
/// is coalesced (the set is keyed by path; see [`SurfacedEntry::absorb`]).
///
/// Returns `(path -> declaring integration and where its write lands)`.
/// Iteration order is sorted by path (`BTreeMap`), matching the deterministic
/// ordering the previous `BTreeSet` provided to symlink creation.
fn compute_owned_set(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> std::collections::BTreeMap<String, SurfacedEntry> {
    let session = WorkspaceSession::new(root);
    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let detection_cache = build_detection_cache(&integrations, root, manifest.iter_entries());
    let ctx_base = session.context_base(project, &detection_cache, manifest.workweave.as_ref());
    let default_config = IntegrationConfig::default();

    let mut owned: std::collections::BTreeMap<String, SurfacedEntry> =
        std::collections::BTreeMap::new();
    for (integration, config) in enabled_integrations(&integrations, manifest, &default_config) {
        let int_ctx = ctx_base.build_context(config, manifest);
        for f in integration
            .generated_files(&int_ctx)
            .into_iter()
            .chain(integration.managed_files(&int_ctx))
        {
            let copy = f.copy();
            let (name, source) = f.into_parts();
            owned
                .entry(name)
                .and_modify(|held| held.absorb(source, copy))
                .or_insert_with(|| SurfacedEntry {
                    integration: integration.name().to_string(),
                    source,
                    copy,
                });
        }
    }
    owned
}

/// The root-relative paths rwv owns for `project`: the
/// `generated_files() ∪ managed_files()` union, without the declaring
/// integration each came from. The path set an intent verb authors, and so
/// the set its commit has to carry.
pub(crate) fn owned_paths(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> BTreeSet<String> {
    compute_owned_set(root, project, manifest)
        .into_keys()
        .collect()
}

/// The owned paths whose link reads the weave's own checkout. A
/// primary-sourced path is absent: every workweave links the primary's copy,
/// so one placed in a workweave's checkout is a copy nothing reads.
pub(crate) fn own_copy_owned_paths(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> BTreeSet<String> {
    compute_owned_set(root, project, manifest)
        .into_iter()
        .filter(|(_, entry)| entry.copy == SurfacedCopy::Weave)
        .map(|(name, _)| name)
        .collect()
}

/// Every file in `project`'s directory that an intent verb's manifest write
/// and its regeneration may change, as it stood before either ran.
///
/// The set is `rwv.toml`, the owned-state ledger, and the owned paths under
/// each manifest given — the one on disk and the one about to be written, since
/// a regeneration rewrites what the new manifest declares and strips what only
/// the old one did. Only regular files are held: a directory or a link at an
/// owned path is a `static-files` source, which no regeneration writes.
pub(crate) struct RestorePoint {
    files: Vec<(std::path::PathBuf, FileBefore)>,
}

enum FileBefore {
    Bytes(Vec<u8>),
    /// With the directories that did not exist either, deepest first.
    Absent(Vec<std::path::PathBuf>),
}

impl RestorePoint {
    pub(crate) fn take(
        root: &Path,
        project: &ProjectName,
        manifests: &[&Manifest],
    ) -> anyhow::Result<Self> {
        let dir = project_dir(root, project.as_str());
        let mut paths: BTreeSet<std::path::PathBuf> =
            BTreeSet::from([dir.join(Manifest::FILE_NAME), ledger_path(&dir)]);
        for manifest in manifests {
            paths.extend(
                owned_paths(root, project, manifest)
                    .into_iter()
                    .map(|file| dir.join(file)),
            );
        }

        let mut files = Vec::new();
        for path in paths {
            let before = match path.symlink_metadata() {
                Ok(meta) if meta.is_file() => {
                    FileBefore::Bytes(std::fs::read(&path).with_context(|| {
                        format!("reading {} before changing it", path.display())
                    })?)
                }
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileBefore::Absent(
                    path.ancestors()
                        .skip(1)
                        .take_while(|dir| dir.symlink_metadata().is_err())
                        .map(Path::to_path_buf)
                        .collect(),
                ),
                Err(e) => {
                    return Err(anyhow::Error::new(e)
                        .context(format!("reading {} before changing it", path.display())))
                }
            };
            files.push((path, before));
        }
        Ok(Self { files })
    }

    /// Put every file back as [`RestorePoint::take`] found it, leaving one that
    /// already matches untouched. Returns the paths it could not put back.
    pub(crate) fn restore(&self) -> Vec<std::path::PathBuf> {
        let mut unrestored = Vec::new();
        for (path, before) in &self.files {
            let restored = match before {
                FileBefore::Bytes(bytes) => {
                    std::fs::read(path).is_ok_and(|now| now == *bytes)
                        || path
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .map_err(anyhow::Error::from)
                            .and_then(|()| crate::durable_file::replace(path, bytes))
                            .is_ok()
                }
                FileBefore::Absent(missing_dirs) => {
                    let removed = !path.symlink_metadata().is_ok_and(|m| m.is_file())
                        || std::fs::remove_file(path).is_ok();
                    for dir in missing_dirs {
                        let _ = std::fs::remove_dir(dir);
                    }
                    removed
                }
            };
            if !restored {
                unrestored.push(path.clone());
            }
        }
        unrestored
    }
}

/// Compute the owner-scoped surfacing set for the project `root` currently
/// presents — the pointer at primary, the marker in a workweave. Returns an
/// empty set if `root` presents no project (in which case no symlinks are
/// owned by rwv and nothing gets removed). This is the deactivate-side
/// analog of step 2 in [`activate_at`].
fn compute_active_owned_set(root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let observed = match observe_root(root) {
        Some(observation) => observation,
        None => return Ok(BTreeSet::new()),
    };
    let active = match observed.presented_project() {
        Some(name) => name.clone(),
        None => return Ok(BTreeSet::new()),
    };
    let manifest_path = project_dir(root, active.as_str()).join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Ok(BTreeSet::new());
    }
    let manifest = Manifest::from_path(&manifest_path)?;
    Ok(owned_paths(root, &active, &manifest))
}

/// Surface the owner-scoped symlink set for `project` into `root` (the
/// **step-2 surfacing primitive**, factored out of `activate_at`).
///
/// This is the re-runnable framework primitive that:
///  1. Computes the `generated_files() ∪ managed_files()` union
///     (`compute_owned_set`).
///  2. Removes stale owner-scoped symlinks (union of the new set with the
///     previously-active project's set, each verified to resolve to
///     `projects/<some-project>/<rel>`).
///  3. Creates the symlinks at `<root>/<file>` pointing at
///     `projects/<project>/<file>`, or for a primary-sourced file in a
///     workweave at the primary's `projects/<project>/<file>`, absolutely.
///
/// It does **not** write `.rwv-active` (that is step-1 project SELECTION, a
/// primary-only concept) and does **not** author integration content. Because
/// it is bound to `root` (the CWD weave directory) rather than to primary, it
/// is valid in any weave — it is exactly what workweave-create runs at
/// creation, and what `rwv doctor --fix` re-runs to repair missing surfacing.
///
/// Whether a link is created over a source that does not exist is the
/// declaration's own answer, not this call's: a
/// [`SurfacedSource::WrittenThroughLink`] path gets its link either way,
/// because the link is what routes the tool's write into the project dir,
/// while a [`SurfacedSource::WrittenAtSource`] path is skipped until its file
/// is there.
///
/// `mode` decides whether `project` may take the root's shared names. In
/// [`SurfacingMode::Repair`] it may only when the root already presents it, so
/// a repair scoped to another project surfaces that project's per-project-named
/// paths and leaves every shared name where the presented project put it.
///
/// All or nothing. Every path a link is wanted at is read before any link
/// moves, so an occupant refuses with the root as it was; a link that already
/// reads its wanted target is left in place rather than re-made; and a failure
/// partway through puts back every link and file this call had changed.
///
/// A real file where a tool writes through its link is not an occupant but the
/// tool's write that missed the link. [`SurfacingMode::Repair`] takes it into
/// the project directory and makes the link — when the project's copy loses
/// nothing by it — and otherwise names both copies and leaves the link out. A
/// selection names it and leaves it, since a context verb writes no project
/// content; the one exception is the outgoing project's file on a switch, which
/// goes home before the incoming project's link can take its path.
pub fn surface_symlinks(
    project_vcs: &dyn Vcs,
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    mode: SurfacingMode,
) -> anyhow::Result<()> {
    for notice in surface_with_notices(project_vcs, root, project, manifest, mode)? {
        eprintln!("[warning] core: surfacing: {notice}");
    }
    Ok(())
}

/// [`surface_symlinks`], returning what it has to say about root writes it left
/// rather than printing it.
fn surface_with_notices(
    project_vcs: &dyn Vcs,
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    mode: SurfacingMode,
) -> anyhow::Result<Vec<String>> {
    let sites = SurfacingSites::of(root);
    let observed = observe_root(root);
    let presented = observed
        .as_ref()
        .and_then(RootObservation::presented_project)
        .cloned();
    let presents_project = mode == SurfacingMode::Select || presented.as_ref() == Some(project);

    // 1. Collect the owner-scoped surfacing set, narrowed to the
    //    per-project-named paths when `project` is not what the root presents.
    let owned = compute_owned_set(root, project, manifest);
    let mut new_owned: BTreeSet<String> = owned.keys().cloned().collect();
    if !presents_project {
        new_owned.retain(|file| is_project_named(file, project));
        let held = compute_active_owned_set(root)?;
        if let Some(collision) = new_owned.iter().find(|file| held.contains(*file)) {
            let presented = presented
                .as_ref()
                .map(ProjectName::as_str)
                .unwrap_or("<none>");
            crate::refuse!(
                RefusalKind::SharedNameContested,
                "surfacing `{collision}` for project `{project}` would take a weave-root name \
                 project `{presented}` also claims; a per-project name cannot be a shared name, \
                 so one of the two projects has to be renamed"
            );
        }
    }
    let wanted: Vec<WantedLink> = owned
        .iter()
        .filter(|(file, _)| new_owned.contains(file.as_str()))
        .filter_map(|(file, entry)| {
            let planned = sites.link(root, project, file, entry.copy)?;
            (!link_waits_for_its_source(entry, &planned.source)).then(|| WantedLink {
                file: file.clone(),
                kind: LinkTarget::on_disk(&planned.source),
                written_through: planned.adopts_root_writes(entry),
                target: planned.target,
            })
        })
        .collect();

    // Selecting a project in place of another: what the outgoing one's tools
    // left at the root is that project's, and goes home before anything of
    // the incoming one's takes its path.
    let outgoing = match (&presented, mode) {
        (Some(previous), SurfacingMode::Select) if previous != project => {
            Some((previous.clone(), written_through_names(root, previous)?))
        }
        _ => None,
    };

    let mut already_linked: BTreeSet<String> = BTreeSet::new();
    let mut occupants = Vec::new();
    let mut root_writes = Vec::new();
    for wanted in &wanted {
        match link_slot(root, &sites, wanted) {
            LinkSlot::AlreadyLinked => {
                already_linked.insert(wanted.file.clone());
            }
            LinkSlot::Clear => {}
            LinkSlot::RootWrite => {
                let owner = match &outgoing {
                    Some((previous, names)) if names.contains(&wanted.file) => previous,
                    _ => project,
                };
                root_writes.extend(crate::root_write::read(root, owner, &wanted.file)?);
            }
            LinkSlot::Occupied(occupant) => occupants.push(occupant),
        }
    }
    if let Some((previous, names)) = &outgoing {
        for name in names {
            if !wanted.iter().any(|wanted| &wanted.file == name) {
                root_writes.extend(crate::root_write::read(root, previous, name)?);
            }
        }
    }
    if !occupants.is_empty() {
        crate::refuse!(
            RefusalKind::SurfacingPathOccupied,
            "surfacing project `{project}` stopped before changing any weave-root link: {}",
            occupants
                .iter()
                .map(|occupant| crate::symlink::occupied_path_remedy(occupant))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

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
    //
    //    The union is what makes the candidate set reach names `project` does
    //    not itself declare, so it is available only to the project the root
    //    presents; for any other project the candidates are its own narrowed
    //    set, or the repair would unlink the presented project's surfacing.
    let mut removal_candidates = if presents_project {
        let mut union = new_owned.clone();
        if let Ok(prev_owned) = compute_active_owned_set(root) {
            for f in prev_owned {
                union.insert(f);
            }
        }
        union
    } else {
        new_owned.clone()
    };
    removal_candidates.retain(|file| !already_linked.contains(file));

    // A root write is adopted where the verb may write — a repair, or taking
    // the outgoing project's file home — and otherwise named and left, with its
    // link left out rather than made over it. An outgoing project's conflicting
    // write refuses the switch instead: left at the root, nothing would record
    // whose it is, and the next project to surface its name would take it in.
    let mut adoption = Vec::new();
    let mut left_unlinked: BTreeSet<String> = BTreeSet::new();
    let mut notices = Vec::new();
    let mut outgoing_conflicts = Vec::new();
    for write in &root_writes {
        let owned_by_outgoing = &write.owner != project;
        if mode != SurfacingMode::Repair && !owned_by_outgoing {
            left_unlinked.insert(write.name.clone());
            notices.push(write.unadopted_message());
            continue;
        }
        match write.verdict(project_vcs, root)? {
            crate::root_write::Verdict::Adopt => adoption.extend(write.adoption(root)),
            crate::root_write::Verdict::Conflict if owned_by_outgoing => {
                outgoing_conflicts.push(write)
            }
            crate::root_write::Verdict::Conflict => {
                left_unlinked.insert(write.name.clone());
                notices.push(write.conflict_message());
            }
        }
    }
    if let Some(first) = outgoing_conflicts.first() {
        crate::refuse!(
            RefusalKind::ConflictingRootWrite,
            "refusing to switch the weave root from project `{}` to `{project}`, and nothing \
             was changed: {}",
            first.owner,
            outgoing_conflicts
                .iter()
                .map(|write| write.switch_refusal_message(project))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    // 2b. Shared names surfaced out of some other project — left behind by a
    //     project switch, or taken by a repair that predates the rule. The
    //     presented project's declarations never reach them, so the removal
    //     cannot.
    let reclaim_from = presents_project.then_some(&new_owned);
    let to_create: Vec<&WantedLink> = wanted
        .iter()
        .filter(|wanted| {
            !already_linked.contains(&wanted.file) && !left_unlinked.contains(&wanted.file)
        })
        .collect();

    let mut journal = SurfacingJournal::default();
    if let Err(failure) = journal.apply(
        root,
        &sites,
        project,
        &adoption,
        &removal_candidates,
        reclaim_from,
        &to_create,
    ) {
        return Err(journal.roll_back(failure));
    }
    Ok(notices)
}

/// The names `project` declares as written through their link, from its own
/// manifest.
fn written_through_names(root: &Path, project: &ProjectName) -> anyhow::Result<BTreeSet<String>> {
    let manifest_path = project_dir(root, project.as_str()).join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Ok(BTreeSet::new());
    }
    let manifest = Manifest::from_path(&manifest_path)?;
    Ok(compute_owned_set(root, project, &manifest)
        .into_iter()
        .filter(|(_, entry)| entry.source == SurfacedSource::WrittenThroughLink)
        .map(|(name, _)| name)
        .collect())
}

/// What a verb that may not adopt says about every root write among
/// `project`'s names: what its own hooks left at the weave root.
fn root_write_notices(root: &Path, project: &ProjectName, manifest: &Manifest) -> Vec<String> {
    compute_owned_set(root, project, manifest)
        .into_iter()
        .filter(|(_, entry)| entry.source == SurfacedSource::WrittenThroughLink)
        .filter_map(
            |(name, _)| match crate::root_write::read(root, project, &name) {
                Ok(write) => write.map(|write| write.unadopted_message()),
                Err(e) => Some(format!(
                    "`{name}` at the weave root could not be read to tell whether a tool \
                     wrote it past its link: {e}"
                )),
            },
        )
        .collect()
}

/// A link surfacing wants at `<root>/<file>`, reading `target`.
struct WantedLink {
    file: String,
    target: std::path::PathBuf,
    kind: LinkTarget,
    written_through: bool,
}

/// What stands at the path a wanted link takes, read before anything moves.
enum LinkSlot {
    /// The wanted link itself.
    AlreadyLinked,
    /// Nothing, or a link of rwv's surfacing shape, which the sweep removes.
    Clear,
    /// A real file where a tool writes through the link: the tool's output,
    /// judged in `crate::root_write` rather than refused.
    RootWrite,
    /// Something no sweep removes, at the path carried — which is an ancestor
    /// of the link's path when that ancestor is not a directory.
    Occupied(std::path::PathBuf),
}

fn link_slot(root: &Path, sites: &SurfacingSites, wanted: &WantedLink) -> LinkSlot {
    let link = root.join(&wanted.file);
    match link.symlink_metadata() {
        Err(e) => unreadable_slot(root, &link, e.kind()),
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(&link) {
            Ok(target) if target == wanted.target => LinkSlot::AlreadyLinked,
            Ok(target) if target_resolves_to_projects(sites, Path::new(&wanted.file), &target) => {
                LinkSlot::Clear
            }
            _ => LinkSlot::Occupied(link),
        },
        Ok(meta) if wanted.written_through && meta.is_file() => LinkSlot::RootWrite,
        Ok(_) => LinkSlot::Occupied(link),
    }
}

/// The slot at `link`, whose metadata could not be read for an error of
/// `kind`. A file standing where a directory belongs is the occupant whatever
/// the error: Unix reports it as `NotADirectory`, Windows as `NotFound`.
fn unreadable_slot(root: &Path, link: &Path, kind: std::io::ErrorKind) -> LinkSlot {
    let file_ancestor = link
        .ancestors()
        .take_while(|path| *path != root)
        .find(|path| path.metadata().is_ok_and(|m| !m.is_dir()));
    match (file_ancestor, kind) {
        (Some(ancestor), _) => LinkSlot::Occupied(ancestor.to_path_buf()),
        (None, std::io::ErrorKind::NotFound) => LinkSlot::Clear,
        (None, _) => LinkSlot::Occupied(link.to_path_buf()),
    }
}

/// Every weave-root change one surfacing pass made, in the order it made them.
#[derive(Default)]
struct SurfacingJournal {
    files: crate::root_write::FileJournal,
    removed: Vec<RemovedLink>,
    created: Vec<std::path::PathBuf>,
    created_dirs: Vec<std::path::PathBuf>,
}

impl SurfacingJournal {
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &mut self,
        root: &Path,
        sites: &SurfacingSites,
        project: &ProjectName,
        adoption: &[crate::root_write::Op],
        removal_candidates: &BTreeSet<String>,
        reclaim_from: Option<&BTreeSet<String>>,
        to_create: &[&WantedLink],
    ) -> anyhow::Result<()> {
        self.files.apply(adoption)?;
        remove_activation_symlinks(root, sites, removal_candidates, &mut self.removed)?;

        if let Some(declared) = reclaim_from {
            for (file, _owner) in foreign_shared_name_links(root, project, declared) {
                let link = root.join(&file);
                let target = std::fs::read_link(&link)?;
                crate::symlink::remove(&link)?;
                self.removed.push(RemovedLink { link, target });
            }
        }

        for wanted in to_create {
            let link = root.join(&wanted.file);
            if let Some(parent) = link.parent() {
                let missing: Vec<std::path::PathBuf> = parent
                    .ancestors()
                    .take_while(|dir| dir.symlink_metadata().is_err())
                    .map(Path::to_path_buf)
                    .collect();
                if !missing.is_empty() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "surfacing `{}`: failed to create parent directory {}",
                            wanted.file,
                            parent.display()
                        )
                    })?;
                    self.created_dirs.extend(missing);
                }
            }
            crate::symlink::create(&wanted.target, &link, wanted.kind)?;
            self.created.push(link);
        }
        Ok(())
    }

    /// Put back what this pass changed, after `failure` stopped it.
    fn roll_back(self, failure: anyhow::Error) -> anyhow::Error {
        let SurfacingJournal {
            files,
            removed,
            created,
            mut created_dirs,
        } = self;
        let mut unrestored: Vec<String> = Vec::new();
        for link in created.iter().rev() {
            if crate::symlink::remove(link).is_err() {
                unrestored.push(link.display().to_string());
            }
        }
        created_dirs.sort_by_key(|dir| std::cmp::Reverse(dir.components().count()));
        for dir in &created_dirs {
            let _ = std::fs::remove_dir(dir);
        }
        for removed in removed.iter().rev() {
            let parent = removed.link.parent().unwrap_or(Path::new(""));
            let kind = LinkTarget::on_disk(&parent.join(&removed.target));
            let restored = std::fs::create_dir_all(parent)
                .map_err(anyhow::Error::from)
                .and_then(|()| crate::symlink::create(&removed.target, &removed.link, kind));
            if restored.is_err() {
                unrestored.push(removed.link.display().to_string());
            }
        }
        unrestored.extend(
            files
                .roll_back()
                .iter()
                .map(|path| path.display().to_string()),
        );
        if unrestored.is_empty() {
            failure.context("surfacing stopped; everything it had changed is back as it was")
        } else {
            failure.context(format!(
                "surfacing stopped, and these could not be put back as they were: {}. \
                 `rwv doctor` reports what is left",
                unrestored.join(", ")
            ))
        }
    }
}

/// The relative symlink target a surfaced file should point at, from the link
/// location (`<root>/<file>`) to the source (`projects/<project>/<file>`).
///
/// For top-level files this is `projects/<project>/<file>`. For nested files
/// like `gita/repos.csv` we prepend `../` once per directory level so the link
/// remains relative. Reached only through [`SurfacingSites::link`], which is
/// what creation and the check both ask.
fn relative_symlink_target(project: &str, file: &str) -> std::path::PathBuf {
    let depth = Path::new(file)
        .parent()
        .map(|p| p.components().count())
        .unwrap_or(0);
    let mut relative_target = std::path::PathBuf::new();
    for _ in 0..depth {
        relative_target.push("..");
    }
    relative_target.push(project_rel_dir(project));
    relative_target.push(file);
    relative_target
}

/// Where the links at one weave root read from, settled once per pass so every
/// reader in it judges against the same answer.
///
/// The absolute shape is recognized as rwv's surfacing only under the root's
/// own primary, and never at a `workweave.link` name: that section
/// canonicalizes its source, and a primary root slot that is itself a
/// surfacing link canonicalizes to exactly this shape.
struct SurfacingSites {
    primary: PrimaryProjects,
    workweave_links: BTreeSet<String>,
}

enum PrimaryProjects {
    /// The root is the primary, so its own `projects/` is the primary's.
    Here,
    /// `<primary>/projects`, canonical and in the spelling `read_link` returns,
    /// for a workweave whose marker witnesses its primary.
    At(std::path::PathBuf),
    /// A marker that witnesses no primary: nothing can be linked from it.
    Unknown,
}

impl PrimaryProjects {
    fn under(primary: &Path, read_back_spelling: fn(&Path) -> &Path) -> Self {
        PrimaryProjects::At(read_back_spelling(primary).join(PROJECTS_DIR))
    }
}

/// One link surfacing makes: what `<root>/<file>` reads, and the file that
/// reaches.
struct PlannedLink {
    target: std::path::PathBuf,
    source: std::path::PathBuf,
    own_copy: bool,
}

impl PlannedLink {
    /// Whether a real file at the link's path is a tool write to take into the
    /// source. Only the weave's own copy may take one; the primary's is not
    /// this weave's to write.
    fn adopts_root_writes(&self, entry: &SurfacedEntry) -> bool {
        self.own_copy && entry.source == SurfacedSource::WrittenThroughLink
    }
}

impl SurfacingSites {
    fn of(root: &Path) -> Self {
        let marker = match observe_root(root) {
            Some(RootObservation::Workweave { marker })
            | Some(RootObservation::Disputed { marker, .. }) => marker,
            Some(RootObservation::MarkerUnverifiable { .. }) => {
                return Self {
                    primary: PrimaryProjects::Unknown,
                    workweave_links: BTreeSet::new(),
                }
            }
            Some(RootObservation::Primary { .. }) | None => {
                return Self {
                    primary: PrimaryProjects::Here,
                    workweave_links: BTreeSet::new(),
                }
            }
        };
        let manifest_path = project_dir(root, marker.project().as_str()).join(Manifest::FILE_NAME);
        let Ok(manifest) = Manifest::from_path(&manifest_path) else {
            return Self {
                primary: PrimaryProjects::Unknown,
                workweave_links: BTreeSet::new(),
            };
        };
        Self {
            primary: PrimaryProjects::under(
                marker.primary_resolved().as_path(),
                crate::path_spelling::link_target,
            ),
            workweave_links: manifest
                .workweave
                .iter()
                .flat_map(|config| &config.link)
                .map(|name| name.trim_end_matches('/').to_string())
                .collect(),
        }
    }

    /// The link for `copy` of `project`'s `file`, or `None` when that copy
    /// cannot be located from this root.
    ///
    /// The one answer [`surface_symlinks`] creates and [`verify_surfacing`]
    /// expects.
    fn link(
        &self,
        root: &Path,
        project: &ProjectName,
        file: &str,
        copy: SurfacedCopy,
    ) -> Option<PlannedLink> {
        match (copy, &self.primary) {
            (SurfacedCopy::Weave, _) | (SurfacedCopy::Primary, PrimaryProjects::Here) => {
                Some(PlannedLink {
                    target: relative_symlink_target(project.as_str(), file),
                    source: project_dir(root, project.as_str()).join(file),
                    own_copy: true,
                })
            }
            (SurfacedCopy::Primary, PrimaryProjects::At(projects)) => {
                let source = projects.join(project.as_str()).join(file);
                Some(PlannedLink {
                    target: source.clone(),
                    source,
                    own_copy: false,
                })
            }
            (SurfacedCopy::Primary, PrimaryProjects::Unknown) => None,
        }
    }

    fn primary_owner_dir(&self, rel_from_root: &Path, target: &Path) -> Option<String> {
        let PrimaryProjects::At(projects) = &self.primary else {
            return None;
        };
        let rel = rel_from_root.to_string_lossy().replace('\\', "/");
        if self.workweave_links.contains(&rel) {
            return None;
        }
        owner_dir_under_projects(rel_from_root, target.strip_prefix(projects).ok()?)
    }
}

/// Whether the surfaced path `file` is named for `project` — `<project>`
/// followed by an extension, the shape `<project>.code-workspace` has. Such a
/// name cannot collide with another project's, so it may sit at the weave root
/// beside the presented project's surfacing.
///
/// Every other surfaced name is SHARED, and shared names follow the project
/// the root presents rather than whichever project a verb was scoped to.
fn is_project_named(file: &str, project: &ProjectName) -> bool {
    file.strip_prefix(project.as_str())
        .is_some_and(|ext| ext.starts_with('.'))
}

/// The project a top-level surfacing symlink named `name` resolves into:
/// `Some(p)` exactly when `target` is `projects/<p>/<name>`, the shape
/// [`relative_symlink_target`] produces for a top-level file. Any other target
/// is not rwv's surfacing of `name`.
fn surfaced_project(target: &Path, name: &str) -> Option<ProjectName> {
    let rest = strip_projects_prefix(target)?;
    let project = rest.parent()?;
    if project.as_os_str().is_empty() || rest.file_name()? != std::ffi::OsStr::new(name) {
        return None;
    }
    ProjectName::new(project.to_str()?).ok()
}

/// A weave-root symlink rwv's surfacing shape claims, at a name the presented
/// project does not declare.
///
/// **The confinement is this type, not a rule someone has to remember.** Its
/// fields are private and it has no public constructor, so the only values that
/// exist are the ones the scan minted on the branch where every conjunct held.
/// [`unsurface_undeclared`] takes these and nothing else,
/// which is what makes "never a file, only a link" a property of the signature:
/// a path that is a regular file cannot be spelled as an argument.
///
/// `target` is the value read at classification time rather than a path to read
/// again. The predicate that decided this link is removable was evaluated
/// against those bytes, and re-reading would decide against a state nobody
/// checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndeclaredLink {
    path: std::path::PathBuf,
    name: String,
    target: std::path::PathBuf,
    owner: String,
}

impl UndeclaredLink {
    /// Root-relative path, `/`-separated.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the link resolved to when it was classified.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// The project directory it resolves into.
    pub fn owner(&self) -> &str {
        &self.owner
    }
}

/// Weave-root symlinks that surface into the PRESENTED project at a name it no
/// longer declares — the residue a dropped declaration leaves, which no verb
/// could see because every candidate set is built from current declarations.
///
/// Four conjuncts, and every one of them is confinement rather than policy:
///
/// 1. the entry is a symlink, read without following it;
/// 2. its target has rwv's own surfacing shape for its own root-relative path
///    ([`surfacing_owner_dir`]) — the predicate the owner-scoped removal
///    already trusts to authorize an unlink;
/// 3. that shape resolves into the project the root presents — the other case
///    belongs to [`foreign_shared_name_links`], and the two partition here
///    rather than overlapping;
/// 4. the name is not in the presented project's declarations, which is the
///    widening: everything else in this file reaches only names rwv currently
///    claims.
///
/// The walk recurses exactly as the removal path does, and skips the same
/// entries, so a nested declaration that left the union (`.cargo/config.toml`
/// when the patch surface changes, `gita/repos.csv` on disablement) is not
/// invisible merely because it sits one directory down.
fn undeclared_project_links(
    root: &Path,
    presented: &ProjectName,
    manifest: &Manifest,
    declared: &BTreeSet<String>,
) -> Vec<UndeclaredLink> {
    let mut exempt = declared.clone();
    exempt.extend(disabled_integration_declarations(root, presented, manifest));
    let skip = sweep_descent_skip(root);
    let sites = SurfacingSites::of(root);
    let mut found = Vec::new();
    collect_undeclared_links_in(root, root, &sites, presented, &exempt, &skip, &mut found);
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Names declared by integrations that are turned OFF for this project.
///
/// Held out of the sweep above, and not as a convenience. Disablement already
/// has a channel and a verb: the disabled-integration pass reports and
/// removes what rwv authored, and [`strip_disabled_links`] unsurfaces every
/// one of these names' weave-root links unconditionally, authored or not —
/// but never the file behind a link it did not author, because that is the
/// operator's own content and is often the reason the integration was turned
/// off. A second finding keyed on the link here would print that same path
/// back out under a different remedy and undo the distinction the
/// disabled-integration pass takes care to make.
fn disabled_integration_declarations(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> BTreeSet<String> {
    let session = WorkspaceSession::new(root);
    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let detection_cache = build_detection_cache(&integrations, root, manifest.iter_entries());
    let ctx_base = session.context_base(project, &detection_cache, manifest.workweave.as_ref());
    let default_config = IntegrationConfig::default();

    integrations
        .iter()
        .filter(|integration| {
            let config = manifest
                .integrations
                .get(integration.name())
                .unwrap_or(&default_config);
            !crate::integration::is_enabled(**integration, config)
        })
        .flat_map(|integration| {
            let config = manifest
                .integrations
                .get(integration.name())
                .unwrap_or(&default_config);
            let ctx = ctx_base.build_context(config, manifest);
            integration
                .generated_files(&ctx)
                .into_iter()
                .chain(integration.managed_files(&ctx))
                .map(|f| f.into_parts().0)
        })
        .collect()
}

fn collect_undeclared_links_in(
    dir: &Path,
    root: &Path,
    sites: &SurfacingSites,
    presented: &ProjectName,
    declared: &BTreeSet<String>,
    skip: &BTreeSet<std::path::PathBuf>,
    found: &mut Vec<UndeclaredLink>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        if meta.file_type().is_dir() {
            if !sweep_skips(&path, skip) {
                collect_undeclared_links_in(&path, root, sites, presented, declared, skip, found);
            }
            continue;
        }
        if !meta.file_type().is_symlink() {
            continue;
        }
        let Ok(rel_from_root) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel_from_root.to_string_lossy().replace('\\', "/");
        if declared.contains(rel.as_str()) {
            continue;
        }
        let Ok(target) = std::fs::read_link(&path) else {
            continue;
        };
        let Some(owner) = surfacing_owner_dir(sites, rel_from_root, &target) else {
            continue;
        };
        if owner != presented.as_str() {
            continue;
        }
        found.push(UndeclaredLink {
            path: path.clone(),
            name: rel,
            target,
            owner,
        });
    }
}

/// Unlink the weave-root symlinks `links` names, and prune directories the
/// removal empties.
///
/// Takes receipts and no root: each link carries the absolute path it was found
/// at, so there is no argument that could redirect this at a different tree, and
/// no path that was not classified. The unlink goes through the typed symlink
/// seam, which removes the link by its own type and never follows it.
pub fn unsurface_undeclared(links: &[UndeclaredLink]) -> anyhow::Result<()> {
    for link in links {
        crate::symlink::remove(&link.path)?;
        if let Some(parent) = link.path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(())
}

/// Weave-root symlinks that surface a SHARED name out of a project other than
/// `presented` — the state the two-class rule forbids, and the residue a
/// repair scoped to the presented project would otherwise not see, because
/// nothing that project declares claims those names.
///
/// Returns `(root name, the project it resolves into)`, sorted by name.
/// Excluded: per-project-named links, which coexist by construction, and the
/// names in `declared`, which the owner-scoped surfacing set already reaches.
fn foreign_shared_name_links(
    root: &Path,
    presented: &ProjectName,
    declared: &BTreeSet<String>,
) -> Vec<(String, ProjectName)> {
    let mut found: Vec<(String, ProjectName)> = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return found,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(target) = std::fs::read_link(&path) else {
            continue;
        };
        let Some(owner) = surfaced_project(&target, name) else {
            continue;
        };
        if &owner == presented || is_project_named(name, &owner) || declared.contains(name) {
            continue;
        }
        found.push((name.to_string(), owner));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Collect the member-incompatibility findings for `project`'s managed files
/// at weave `root`, against the same project-dir view intent-mode activation
/// authors into.
///
/// A thin binding of
/// [`run_member_incompatibilities`](crate::integration_runner::run_member_incompatibilities)
/// to a weave directory, for callers that hold a `root` + manifest rather than
/// an `IntegrationContextBase` — `rwv update`'s post-MOVE report. `rwv doctor`
/// calls the runner directly with the context base it already built; both reach
/// the same per-integration predicate, so the two surfacings cannot diverge.
///
/// `root` is the weave the caller operated on (primary or a workweave), matching
/// [`activate_intent`] / [`activate_workweave_intent`]. Reports only — nothing
/// here refuses or repairs.
pub fn member_incompatibilities(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> Vec<crate::integration::Issue> {
    let session = WorkspaceSession::new(root);
    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let detection_cache = build_detection_cache(&integrations, root, manifest.iter_entries());
    let ctx_base = session.context_base(project, &detection_cache, manifest.workweave.as_ref());
    crate::integration_runner::run_member_incompatibilities(&integrations, manifest, &ctx_base)
}

/// Framework-level **Axis-1 surfacing** check: assert that every file in the
/// owner-scoped surfacing union exists at `<root>/<file>` as a symlink that
/// resolves to `projects/<project>/<file>` — the primary's, absolutely, for a
/// primary-sourced file in a workweave.
///
/// It reads the same `generated_files() ∪ managed_files()` union that drives
/// symlink creation (`compute_owned_set`) — it lives in the
/// framework and is byte-identical across all integrations, so it is NOT
/// duplicated into per-integration `verify()` bodies (those own Axis-2 content
/// drift). Any divergence between an integration's declared surfacing set and
/// the on-disk symlinks (manual `rm`, interrupted create, a manifest change
/// that adds a file, enabling an integration in an existing workweave) is
/// invisible to the per-integration verify pass; this closes that gap.
///
/// Emits one `Severity::Warning`, `safe_to_fix=true` `Issue` per missing or
/// mis-resolved symlink. The recovery hatch is `rwv doctor --fix`, which calls
/// [`surface_symlinks`] bound to the weave that was scanned — an Axis-1 gap
/// needs no authoring, and [`activate_intent`] would target primary.
///
/// Expectations mirror [`surface_symlinks`] through the one predicate both
/// consult, so the check expects exactly what the creator makes. A link
/// standing over an absent file therefore reads two
/// ways by declaration rather than by verb: pending for a path a tool writes
/// through it, stale for a path whose file should already be there.
///
/// The expectations are likewise symmetric with what a repair may create: a
/// project the root does not present is expected to surface its
/// per-project-named paths and nothing else, and the project the root DOES
/// present is additionally expected to hold every shared name at the root,
/// including names it does not itself declare.
pub fn verify_surfacing(
    project_vcs: &dyn Vcs,
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> Vec<crate::integration::Issue> {
    use crate::integration::{Issue, IssueKind, Severity};

    let sites = SurfacingSites::of(root);
    let presents_project = observe_root(root)
        .as_ref()
        .and_then(RootObservation::presented_project)
        == Some(project);
    let owned = compute_owned_set(root, project, manifest);
    let mut issues = Vec::new();

    if presents_project {
        let declared: BTreeSet<String> = owned.keys().cloned().collect();
        for link in undeclared_project_links(root, project, manifest, &declared) {
            issues.push(Issue {
                integration: "core".into(),
                severity: Severity::Warning,
                message: format!(
                    "surfacing: `{}` is a weave-root link into project `{}` at a name \
                     `{}` no longer declares (it resolves to `{}`). Not auto-fixed: on \
                     disk this is indistinguishable from a link you made by hand at the \
                     same shape, so `rwv doctor --fix` leaves it. Remove it with \
                     `rwv materialize --remove-undeclared-links`, which removes exactly \
                     the links reported here and nothing else; the file it points at is \
                     untouched either way.",
                    link.name(),
                    link.owner(),
                    project,
                    crate::path_spelling::weave_relative(link.target())
                ),
                kind: IssueKind::Surfacing,
                safe_to_fix: false,
            });
        }
        for (file, owner) in foreign_shared_name_links(root, project, &declared) {
            issues.push(Issue {
                integration: "core".into(),
                severity: Severity::Warning,
                message: format!(
                    "surfacing: `{file}` resolves into project `{owner}` while the weave root \
                     presents `{project}` (shared names follow the root's project; safe to --fix)"
                ),
                kind: IssueKind::Surfacing,
                safe_to_fix: true,
            });
        }
    }

    for (file, entry) in &owned {
        if !presents_project && !is_project_named(file, project) {
            continue;
        }
        let Some(planned) = sites.link(root, project, file, entry.copy) else {
            continue;
        };
        let source = &planned.source;
        let source_missing = link_waits_for_its_source(entry, source);
        let integration = &entry.integration;

        let link = root.join(file);
        let expected_target = &planned.target;

        let link_meta = match link.symlink_metadata() {
            Ok(m) => m,
            Err(_) => {
                // Mirror the create path: a file whose source is absent in a
                // workweave is intentionally not surfaced, so an absent
                // symlink is not flagged.
                if source_missing {
                    continue;
                }
                issues.push(Issue {
                    integration: integration.clone(),
                    severity: Severity::Warning,
                    message: format!(
                        "surfacing: `{file}` is not surfaced (no symlink at `{}`; safe to --fix)",
                        link.display()
                    ),
                    kind: IssueKind::Surfacing,
                    safe_to_fix: true,
                });
                continue;
            }
        };

        if planned.adopts_root_writes(entry) && link_meta.is_file() {
            let judged = match crate::root_write::read(root, project, file) {
                Ok(None) => None,
                Ok(Some(write)) => Some(
                    write
                        .verdict(project_vcs, root)
                        .map(|verdict| (verdict, write))
                        .map_err(anyhow::Error::from),
                ),
                Err(e) => Some(Err(e.into())),
            };
            if let Some(judged) = judged {
                let (severity, message, safe_to_fix) = match judged {
                    Ok((crate::root_write::Verdict::Adopt, write)) => {
                        (Severity::Warning, write.unadopted_message(), true)
                    }
                    Ok((crate::root_write::Verdict::Conflict, write)) => {
                        (Severity::Warning, write.conflict_message(), false)
                    }
                    Err(e) => (
                        Severity::Error,
                        format!(
                            "`{file}` at the weave root is a real file where its link belongs, \
                             and whether taking it into the project loses anything could not be \
                             determined: {e:#}"
                        ),
                        false,
                    ),
                };
                issues.push(Issue {
                    integration: integration.clone(),
                    severity,
                    message: format!("surfacing: {message}"),
                    kind: IssueKind::Surfacing,
                    safe_to_fix,
                });
                continue;
            }
        }

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
                kind: IssueKind::Surfacing,
                safe_to_fix: false,
            });
            continue;
        }

        match std::fs::read_link(&link) {
            Ok(actual) if actual == *expected_target => {
                if source_missing {
                    issues.push(Issue {
                        integration: integration.clone(),
                        severity: Severity::Warning,
                        message: format!(
                            "surfacing: `{file}` is surfaced but its source no longer exists \
                             at `{}` (stale symlink; safe to --fix)",
                            source.display()
                        ),
                        kind: IssueKind::Surfacing,
                        safe_to_fix: true,
                    });
                }
            }
            Ok(actual) => {
                let replaceable = target_resolves_to_projects(&sites, Path::new(file), &actual);
                issues.push(Issue {
                    integration: integration.clone(),
                    severity: Severity::Warning,
                    message: format!(
                        "surfacing: `{file}` symlink resolves to `{}` (expected `{}`; {})",
                        actual.display(),
                        expected_target.display(),
                        if replaceable {
                            "safe to --fix"
                        } else {
                            "not rwv's surfacing link, so not auto-fixed; remove it and \
                             `rwv doctor --fix` makes the expected one"
                        }
                    ),
                    kind: IssueKind::Surfacing,
                    safe_to_fix: replaceable,
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
                    kind: IssueKind::Surfacing,
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
    use crate::integration::{Issue, IssueKind};
    use crate::vcs::project_vcs;

    fn issue(integration: &str, severity: Severity, message: &str) -> Issue {
        Issue {
            integration: integration.into(),
            severity,
            message: message.into(),
            kind: IssueKind::ToolMissing,
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
    // Repair-verb naming
    // -----------------------------------------------------------------------

    #[test]
    fn hook_that_failed_while_running_names_doctor_fix() {
        // Partial-activation errors (from install hooks like `npm install`,
        // `cargo generate-lockfile`) must name `rwv doctor --fix` as the
        // repair verb — re-running activate does NOT re-run hooks and cannot
        // self-heal. This test asserts the house pattern:
        //   state → verb → escape hatch.
        //
        // Scoped to a hook that ran and failed, which is the only cause that
        // reaches this reporter: `refuse_hooks_over_malformed_settings` takes
        // the settings-parse cause before any hook is called, and re-running
        // the generators repairs nothing there.
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

    /// A weave whose one project hands `[integrations.cargo-workspace]` the
    /// given body, ready for a context-mode activation with the hooks live.
    fn make_cargo_settings_workspace(settings_body: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("github")).unwrap();
        let project_dir = root.join("projects").join("demo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join(Manifest::FILE_NAME),
            format!("[repositories]\n\n[integrations.cargo-workspace]\n{settings_body}\n"),
        )
        .unwrap();
        std::fs::write(root.join(".rwv-active"), "demo\n").unwrap();
        tmp
    }

    fn activate_context_with_hooks(root: &Path) -> anyhow::Result<()> {
        activate_at(
            root,
            "demo",
            ActivateOptions {
                no_materialize: false,
            },
            ActivationMode::Context,
        )
    }

    #[test]
    fn settings_that_do_not_parse_send_the_operator_to_the_manifest() {
        // The command operators run takes the context path, whose check and
        // verify passes report and continue. Reaching the hooks with a block
        // that yielded no settings means a raw bail captured as a generic hook
        // failure, and the repair that capture advises — regenerate, re-run the
        // hooks — has to read the block it just failed to read. So the outcome
        // asserted here is the operator-facing one: the run names the edit that
        // fixes it and never names the regeneration that cannot.
        let tmp = make_cargo_settings_workspace("exclude = \"not-a-list\"");
        let err = activate_context_with_hooks(tmp.path()).unwrap_err();
        let msg = err.to_string();

        // The prohibition first, so that whatever removes the guard is reported
        // against the sentence the operator must never see rather than against
        // whichever positive assertion happens to be listed above it.
        assert!(
            !msg.contains("doctor --fix"),
            "a regeneration cannot read a block that does not parse, so the \
             operator must not be sent to one; got: {msg}"
        );
        assert!(
            msg.contains("Edit rwv.toml"),
            "the remedy is an edit to the manifest, and the operator has to be \
             told to make it — a mention of the filename inside a finding is not \
             an instruction; got: {msg}"
        );
        assert!(
            msg.contains("install hooks were not run"),
            "the run must say the hooks were withheld, not that they failed; \
             got: {msg}"
        );

        // The same weave with a well-formed block activates: what the arm above
        // captured is the settings, not a fixture this activation cannot run
        // against at all.
        let control = make_cargo_settings_workspace("exclude = []");
        activate_context_with_hooks(control.path()).unwrap();
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
    // surface_symlinks + verify_surfacing (Axis-1 surfacing)
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
        let project = write_surfacing_project(root, "web-app", declared, authored);

        // A primary root that presents this project. Shared names follow the
        // pointer, so without it every surfacing call below would be scoped to
        // a project the root does not present.
        std::fs::write(root.join(".rwv-active"), format!("{project}\n")).unwrap();

        (tmp, project)
    }

    /// Write a second project into an existing fixture root, so a test can
    /// scope surfacing to a project the root does not present.
    fn write_surfacing_project(
        root: &Path,
        project: &str,
        declared: &[&str],
        authored: &[&str],
    ) -> ProjectName {
        let project_dir = root.join("projects").join(project);
        std::fs::create_dir_all(&project_dir).unwrap();

        let files_list = declared
            .iter()
            .map(|f| format!("\"{f}\""))
            .collect::<Vec<_>>()
            .join(", ");
        // Disable the default-enabled integrations that declare files
        // unconditionally (vscode-workspace surfaces `<project>.code-workspace`,
        // go-work surfaces `go.work.sum`) so the surfacing union under test is
        // exactly the static-files set we declare. The surfacing machinery is
        // integration-agnostic; isolating one integration keeps the asserts
        // deterministic.
        let manifest_toml = format!(
            "[repositories]\n\
             \n\
             [integrations.static-files]\n\
             enabled = true\n\
             files = [{files_list}]\n\
             \n\
             [integrations.vscode-workspace]\n\
             enabled = false\n\
             \n\
             [integrations.go-work]\n\
             enabled = false\n"
        );
        std::fs::write(project_dir.join(Manifest::FILE_NAME), &manifest_toml).unwrap();

        // Author the requested files in the project dir so they can be surfaced.
        for f in authored {
            let p = project_dir.join(f);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, format!("content of {f}\n")).unwrap();
        }

        ProjectName::new(project).unwrap()
    }

    fn load_manifest(root: &Path, project: &ProjectName) -> Manifest {
        let path = root
            .join("projects")
            .join(project.as_str())
            .join(Manifest::FILE_NAME);
        Manifest::from_path(&path).unwrap()
    }

    #[test]
    fn a_link_that_cannot_be_created_refuses_instead_of_warning() {
        // A real file at the surfacing path survives the owner-scoped removal,
        // which unlinks symlinks only, so link creation hits EEXIST. An Ok
        // return with `.claude` unsurfaced reads to every consumer as a
        // project that never declared the file.
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);
        std::fs::write(root.join(".claude"), "user content").unwrap();

        let err = surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &manifest,
            SurfacingMode::Repair,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(".claude"),
            "the refusal must name the link it could not create: {msg}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".claude")).unwrap(),
            "user content"
        );
    }

    /// Link `files` at the root as surfacing would, out of `project`.
    fn link_as_surfaced(root: &Path, project: &str, files: &[&str]) {
        for file in files {
            let link = root.join(file);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            crate::symlink::create(
                &relative_symlink_target(project, file),
                &link,
                LinkTarget::File,
            )
            .unwrap();
        }
    }

    fn read_links(root: &Path, files: &[&str]) -> Vec<Option<std::path::PathBuf>> {
        files
            .iter()
            .map(|file| std::fs::read_link(root.join(file)).ok())
            .collect()
    }

    #[test]
    fn an_occupied_path_leaves_every_surfaced_link_in_place() {
        // A real file occupying one surfaced path must not cost the links
        // that sort after it.
        let surfaced = [
            ".claude",
            "AGENTS.md",
            "package.json",
            "web-app.code-workspace",
        ];
        let mut declared = surfaced.to_vec();
        declared.push("package-lock.json");
        let (tmp, project) = make_surfacing_workspace(&declared);
        let root = tmp.path();
        link_as_surfaced(root, project.as_str(), &surfaced);
        std::fs::write(root.join("package-lock.json"), "{}\n").unwrap();
        let before = read_links(root, &surfaced);

        let err = surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
            SurfacingMode::Repair,
        )
        .unwrap_err();

        assert_eq!(
            crate::refusal::kind_of(&err),
            Some(RefusalKind::SurfacingPathOccupied),
            "{err:#}"
        );
        assert_eq!(read_links(root, &surfaced), before);
        assert_eq!(
            std::fs::read_to_string(root.join("package-lock.json")).unwrap(),
            "{}\n"
        );
    }

    #[test]
    fn an_occupied_path_refusal_names_every_occupant() {
        // Read before anything moves, so the operator clears them all in one
        // pass instead of meeting them one re-run at a time.
        let (tmp, project) =
            make_surfacing_workspace(&[".claude", "AGENTS.md", "package-lock.json"]);
        let root = tmp.path();
        link_as_surfaced(root, "old-app", &[".claude"]);
        std::fs::write(root.join("AGENTS.md"), "mine\n").unwrap();
        std::fs::write(root.join("package-lock.json"), "{}\n").unwrap();

        let msg = surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
            SurfacingMode::Repair,
        )
        .unwrap_err()
        .to_string();

        assert!(
            msg.contains("AGENTS.md") && msg.contains("package-lock.json"),
            "every occupant must be named: {msg}"
        );
        assert_eq!(
            std::fs::read_link(root.join(".claude")).unwrap(),
            Path::new("projects/old-app/.claude"),
            "nothing may move before the occupants are cleared"
        );
    }

    #[test]
    fn an_ancestor_that_is_not_a_directory_is_the_occupant_named() {
        let (tmp, project) = make_surfacing_workspace(&["gita/repos.csv"]);
        let root = tmp.path();
        std::fs::write(root.join("gita"), "a file\n").unwrap();

        let msg = surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
            SurfacingMode::Repair,
        )
        .unwrap_err()
        .to_string();

        assert!(
            msg.contains(&format!("{} (a regular file)", root.join("gita").display())),
            "the refusal must name the file standing where a directory belongs: {msg}"
        );
    }

    #[test]
    fn a_file_ancestor_is_the_occupant_whichever_error_the_platform_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("gita"), "a file\n").unwrap();
        let link = root.join("gita/repos.csv");

        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::NotADirectory,
        ] {
            assert!(
                matches!(
                    unreadable_slot(root, &link, kind),
                    LinkSlot::Occupied(ref at) if *at == root.join("gita")
                ),
                "{kind:?} under a file did not name the file"
            );
        }
    }

    #[test]
    fn a_path_missing_under_directories_is_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("projects/old/.claude")).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("projects/old/.claude", root.join(".claude")).unwrap();
            std::os::unix::fs::symlink("projects/gone/.agents", root.join(".agents")).unwrap();
        }

        for missing in ["notes.md", "sub/notes.md", ".claude/x.md", ".agents/x.md"] {
            assert!(
                matches!(
                    unreadable_slot(root, &root.join(missing), std::io::ErrorKind::NotFound),
                    LinkSlot::Clear
                ),
                "{missing} was read as occupied"
            );
        }
    }

    /// Mode bits only obstruct an unprivileged process, and these tests assert
    /// on the obstruction.
    #[cfg(unix)]
    fn read_only(dir: &Path) -> std::fs::Permissions {
        use std::os::unix::fs::PermissionsExt;
        let writable = std::fs::metadata(dir).unwrap().permissions();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        writable
    }

    #[test]
    #[cfg(unix)]
    fn a_link_already_at_its_target_is_left_alone() {
        // Observed through a directory nothing may be unlinked from: a pass
        // that re-made the link would have to.
        let (tmp, project) = make_surfacing_workspace(&["sub/notes.md"]);
        let root = tmp.path();
        link_as_surfaced(root, project.as_str(), &["sub/notes.md"]);
        let writable = read_only(&root.join("sub"));

        let result = surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
            SurfacingMode::Repair,
        );
        std::fs::set_permissions(root.join("sub"), writable).unwrap();

        result.expect("a link already at its target needs no write to stay surfaced");
        assert_eq!(
            std::fs::read_link(root.join("sub/notes.md")).unwrap(),
            Path::new("../projects/web-app/sub/notes.md")
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_failure_after_links_moved_puts_them_back() {
        // No occupant, so the pass starts: `.claude` is replaced, then the
        // link under the read-only `sub/` cannot be made.
        let (tmp, project) = make_surfacing_workspace(&[".claude", "sub/notes.md"]);
        let root = tmp.path();
        link_as_surfaced(root, "old-app", &[".claude"]);
        std::fs::create_dir(root.join("sub")).unwrap();
        let writable = read_only(&root.join("sub"));

        let result = surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
            SurfacingMode::Repair,
        );
        std::fs::set_permissions(root.join("sub"), writable).unwrap();

        assert!(result.is_err(), "the obstruction must stop the pass");
        assert_eq!(
            std::fs::read_link(root.join(".claude")).unwrap(),
            Path::new("projects/old-app/.claude"),
            "the link the pass replaced before failing must be put back"
        );
    }

    #[test]
    fn a_sweep_descends_into_a_nested_directory_named_like_a_registry() {
        // `github` and `projects` are the registry and the project tree only
        // at the top of the weave.
        let (tmp, project) = make_surfacing_workspace(&["docs/github/notes.md"]);
        let root = tmp.path();
        link_as_surfaced(root, project.as_str(), &["docs/github/notes.md"]);
        link_as_surfaced(root, project.as_str(), &["docs/projects/notes.md"]);

        let issues = verify_surfacing(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.message.contains("docs/projects/notes.md")),
            "an undeclared link under a nested `projects/` must be reported: {issues:?}"
        );

        unsurface_names(root, &["docs/github/notes.md".to_string()]).unwrap();
        assert!(
            root.join("docs/github/notes.md")
                .symlink_metadata()
                .is_err(),
            "a declared link under a nested `github/` must be swept"
        );
    }

    #[test]
    fn a_sweep_still_stops_at_a_top_level_registry_directory() {
        let (tmp, _project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        link_as_surfaced(root, "web-app", &["github/acme/notes.md"]);

        unsurface_names(root, &["github/acme/notes.md".to_string()]).unwrap();

        assert!(root
            .join("github/acme/notes.md")
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink()));
    }

    #[test]
    fn a_sweep_prunes_only_directories_it_emptied() {
        let (tmp, project) = make_surfacing_workspace(&["gita/repos.csv"]);
        let root = tmp.path();
        link_as_surfaced(root, project.as_str(), &["gita/repos.csv"]);
        std::fs::create_dir(root.join("scratch")).unwrap();

        unsurface_names(root, &["gita/repos.csv".to_string()]).unwrap();

        assert!(
            root.join("scratch").is_dir(),
            "an empty directory the sweep removed nothing from is the operator's"
        );
        assert!(
            root.join("gita").symlink_metadata().is_err(),
            "the directory the removal emptied goes with it"
        );
    }

    #[test]
    fn a_restore_point_puts_back_what_a_regeneration_changed() {
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let dir = project_dir(root, project.as_str());
        let manifest = load_manifest(root, &project);
        let mut edited = manifest.clone();
        edited.integrations.insert(
            "static-files".to_string(),
            toml::from_str("enabled = true\nfiles = [\".claude\", \"nested/new.md\"]").unwrap(),
        );
        let manifest_before = std::fs::read(dir.join(Manifest::FILE_NAME)).unwrap();

        let restore_point = RestorePoint::take(root, &project, &[&manifest, &edited]).unwrap();
        edited.write(&dir.join(Manifest::FILE_NAME)).unwrap();
        std::fs::create_dir(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/new.md"), "made by the failed run\n").unwrap();
        std::fs::write(dir.join(".claude"), "rewritten\n").unwrap();

        assert_eq!(restore_point.restore(), Vec::<std::path::PathBuf>::new());

        assert_eq!(
            std::fs::read(dir.join(Manifest::FILE_NAME)).unwrap(),
            manifest_before
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(".claude")).unwrap(),
            "content of .claude\n"
        );
        assert!(
            !dir.join("nested").exists(),
            "a file the run created goes, with the directory made for it"
        );
    }

    #[test]
    fn a_directory_source_is_surfaced() {
        // `.beads` and `.claude` are surfaced directories in a real weave, and
        // Windows creates a link to a directory with a different call than a
        // link to a file.
        let (tmp, project) = make_surfacing_workspace_authoring(&[".claude"], &[]);
        let root = tmp.path();
        let claude = project_dir(root, project.as_str()).join(".claude");
        std::fs::create_dir_all(claude.join("agents")).unwrap();
        std::fs::write(claude.join("agents").join("a.md"), "x").unwrap();
        let manifest = load_manifest(root, &project);

        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &manifest,
            SurfacingMode::Repair,
        )
        .unwrap();

        let link = root.join(".claude");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(
            link.join("agents").join("a.md").is_file(),
            "the link must resolve as a directory"
        );
    }

    #[test]
    fn surface_then_verify_is_clean() {
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &manifest,
            SurfacingMode::Repair,
        )
        .unwrap();

        // The symlink exists at the root and resolves to the project copy.
        let link = root.join(".claude");
        let meta = link.symlink_metadata().unwrap();
        assert!(meta.file_type().is_symlink(), ".claude should be a symlink");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("projects/web-app/.claude")
        );

        // verify_surfacing reports nothing — surfacing matches the union.
        let issues = verify_surfacing(project_vcs().as_ref(), root, &project, &manifest);
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
        let issues = verify_surfacing(project_vcs().as_ref(), root, &project, &manifest);
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
        // Nothing follows this link — `verify_surfacing` reads it with
        // `symlink_metadata` and `read_link` — so the kind is immaterial here
        // and takes the absent-target rule.
        crate::symlink::create(
            Path::new("projects/other/.claude"),
            &root.join(".claude"),
            LinkTarget::File,
        )
        .unwrap();

        let issues = verify_surfacing(project_vcs().as_ref(), root, &project, &manifest);
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

        let issues = verify_surfacing(project_vcs().as_ref(), root, &project, &manifest);
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
        assert_eq!(
            verify_surfacing(project_vcs().as_ref(), root, &project, &manifest).len(),
            1
        );

        // The fix primitive (NOT activate_intent) creates the symlink.
        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &manifest,
            SurfacingMode::Repair,
        )
        .unwrap();
        assert!(
            verify_surfacing(project_vcs().as_ref(), root, &project, &manifest).is_empty(),
            "re-surfacing should clear the missing-symlink finding"
        );
    }

    #[test]
    fn surface_does_not_write_rwv_active() {
        // The factored primitive is step-2 ONLY: it must not perform step-1
        // project SELECTION (.rwv-active write), which is a primary-only
        // concept and must not be dragged into a workweave. Surfacing a
        // project the root does NOT present is where a selection write would
        // be visible, and Select is the mode that would license one.
        let (tmp, project) = make_surfacing_workspace(&[".claude"]);
        let root = tmp.path();
        let other = write_surfacing_project(root, "other-app", &[".claude"], &[".claude"]);
        let manifest = load_manifest(root, &other);

        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &other,
            &manifest,
            SurfacingMode::Select,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(".rwv-active"))
                .unwrap()
                .trim(),
            project.as_str(),
            "surface_symlinks must not write .rwv-active (that is step-1 selection)"
        );
    }

    /// A declared file written at its source, with no source yet, is not
    /// expected to be surfaced anywhere — the answer no longer depends on
    /// which verb is asking.
    #[test]
    fn verify_does_not_expect_a_link_for_a_source_that_is_not_there() {
        let (tmp, project) = make_surfacing_workspace_authoring(&[".claude"], &[]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        assert!(verify_surfacing(project_vcs().as_ref(), root, &project, &manifest).is_empty());
    }

    /// A weave with one cargo member and nothing authored in the project dir,
    /// so the surfacing union holds `Cargo.lock` (written through its link)
    /// and `Cargo.toml` (written at its source) with BOTH sources absent.
    ///
    /// The fixture exists to stop provenance being tested one arm at a time:
    /// every input the two paths could differ on is held equal here, so the
    /// declaration is the only thing left that can explain a difference in
    /// outcome. `cargo` is never invoked — the declaration gate is a
    /// filesystem test for a member manifest.
    fn make_cargo_surfacing_workspace() -> (tempfile::TempDir, ProjectName) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let member = root.join("github/acme/server");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let project_dir = root.join("projects").join("web-app");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join(Manifest::FILE_NAME),
            "[repositories.\"github/acme/server\"]\n\
             type = \"git\"\n\
             url = \"https://example.com/server.git\"\n\
             version = \"main\"\n\
             role = \"owned\"\n\
             \n\
             [integrations.vscode-workspace]\n\
             enabled = false\n\
             \n\
             [integrations.go-work]\n\
             enabled = false\n",
        )
        .unwrap();
        std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();
        (tmp, ProjectName::new("web-app").unwrap())
    }

    /// The two rows the provenance signal exists for, driven from one fixture
    /// so the difference cannot be an artifact of anything else.
    ///
    /// Catches the loop directly: before the signal, `Cargo.lock`'s dangling
    /// link was created by one verb and reported stale by the next, and
    /// `Cargo.toml` got a dangling link nothing would ever fill.
    #[test]
    fn a_missing_source_means_opposite_things_for_the_two_provenances() {
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &manifest,
            SurfacingMode::Repair,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_link(root.join("Cargo.lock")).ok(),
            Some(std::path::PathBuf::from("projects/web-app/Cargo.lock")),
            "the lock's link is the route cargo's write takes into the project \
             dir, so it must exist before the file does"
        );
        assert!(
            root.join("Cargo.toml").symlink_metadata().is_err(),
            "nothing writes the managed manifest through its link, so a link \
             over its absent source would dangle forever"
        );

        let issues = verify_surfacing(project_vcs().as_ref(), root, &project, &manifest);
        assert!(
            issues.is_empty(),
            "surfacing just produced this state, so the check that reads it \
             must agree: {issues:?}"
        );
    }

    /// The pending link survives the repair that used to remove it. One round
    /// of the loop, which is all it took to never converge.
    #[test]
    fn repairing_twice_leaves_the_pending_lock_link_alone() {
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        for round in 1..=3 {
            surface_symlinks(
                project_vcs().as_ref(),
                root,
                &project,
                &manifest,
                SurfacingMode::Repair,
            )
            .unwrap();
            assert!(
                root.join("Cargo.lock").symlink_metadata().is_ok(),
                "round {round}: the pending link was removed"
            );
            let issues = verify_surfacing(project_vcs().as_ref(), root, &project, &manifest);
            assert!(
                issues.is_empty(),
                "round {round}: a repair left something for the next check to \
                 report, which is the loop: {issues:?}"
            );
        }
    }

    fn write_root_lock(root: &Path, bytes: &str) {
        std::fs::write(root.join("Cargo.lock"), bytes).unwrap();
    }

    fn write_project_lock(root: &Path, project: &str, bytes: &str) {
        std::fs::write(
            root.join("projects").join(project).join("Cargo.lock"),
            bytes,
        )
        .unwrap();
    }

    /// `project`'s directory made a repo with `bytes` committed as its lock.
    fn commit_project_lock(root: &Path, project: &str, bytes: &str) {
        let repo = root.join("projects").join(project);
        write_project_lock(root, project, bytes);
        for args in [
            &["init", "-q"][..],
            &["add", "Cargo.lock"],
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=T",
                "commit",
                "-qm",
                "lock",
            ],
        ] {
            let out = crate::git::git_command_in_test()
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn is_lock_link(root: &Path, project: &str) -> bool {
        std::fs::read_link(root.join("Cargo.lock")).ok()
            == Some(std::path::PathBuf::from(format!(
                "projects/{project}/Cargo.lock"
            )))
    }

    fn surface(root: &Path, project: &ProjectName, mode: SurfacingMode) -> anyhow::Result<()> {
        surface_symlinks(
            project_vcs().as_ref(),
            root,
            project,
            &load_manifest(root, project),
            mode,
        )
    }

    #[test]
    fn a_lock_written_in_place_reaches_the_project_through_its_link_at_once() {
        // How cargo, uv and npm 10 write: open the path and write it, first
        // generation through the dangling link and every later one alike.
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        surface(root, &project, SurfacingMode::Repair).unwrap();
        assert!(!root.join("projects/web-app/Cargo.lock").exists());

        for generation in ["version = 4\n", "version = 4\n# regenerated\n"] {
            std::fs::write(root.join("Cargo.lock"), generation).unwrap();

            assert_eq!(
                read(&root.join("projects/web-app/Cargo.lock")).as_deref(),
                Some(generation),
                "the write must land in the project with no verb run after it"
            );
            assert!(is_lock_link(root, "web-app"));
        }
        let issues = verify_surfacing(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
        );
        assert!(issues.is_empty(), "nothing is left to adopt: {issues:?}");
    }

    #[test]
    fn a_first_lock_written_past_its_dangling_link_is_adopted_and_relinked() {
        // What `pnpm install` leaves on a first generation: the link dangled,
        // pnpm renamed its file over it, and the project has no lock.
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        write_root_lock(root, "version = 4\n");

        surface(root, &project, SurfacingMode::Repair).unwrap();

        assert!(is_lock_link(root, "web-app"));
        assert_eq!(
            read(&root.join("projects/web-app/Cargo.lock")).as_deref(),
            Some("version = 4\n")
        );
    }

    #[test]
    fn a_root_write_identical_to_the_projects_is_relinked() {
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        write_project_lock(root, "web-app", "version = 4\n");
        write_root_lock(root, "version = 4\n");

        surface(root, &project, SurfacingMode::Repair).unwrap();

        assert!(is_lock_link(root, "web-app"));
    }

    #[test]
    fn a_root_write_over_a_committed_project_copy_is_adopted() {
        // A tool ran where the link was missing: history holds the project
        // copy, so taking the tool's bytes over it loses nothing.
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        commit_project_lock(root, "web-app", "# committed\n");
        write_root_lock(root, "# regenerated\n");

        surface(root, &project, SurfacingMode::Repair).unwrap();

        assert!(is_lock_link(root, "web-app"));
        assert_eq!(
            read(&root.join("projects/web-app/Cargo.lock")).as_deref(),
            Some("# regenerated\n")
        );
    }

    #[test]
    fn a_root_write_over_the_recorded_project_copy_is_adopted() {
        // No commit holds the project copy, but it is still the generation rwv
        // recorded, so taking the tool's bytes loses nothing rwv cannot remake.
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        write_project_lock(root, "web-app", "# accepted\n");
        stamp_owned_digest(
            &root.join("projects/web-app"),
            "Cargo.lock",
            b"# accepted\n",
        )
        .unwrap();
        write_root_lock(root, "# regenerated\n");

        surface(root, &project, SurfacingMode::Repair).unwrap();

        assert!(is_lock_link(root, "web-app"));
        assert_eq!(
            read(&root.join("projects/web-app/Cargo.lock")).as_deref(),
            Some("# regenerated\n")
        );
    }

    #[test]
    fn a_root_write_over_an_uncommitted_project_copy_leaves_both_and_the_link_out() {
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        commit_project_lock(root, "web-app", "# committed\n");
        write_project_lock(root, "web-app", "# edited\n");
        write_root_lock(root, "# the tool's\n");

        surface(root, &project, SurfacingMode::Repair).expect("a conflict is named, not refused");

        assert_eq!(
            read(&root.join("Cargo.lock")).as_deref(),
            Some("# the tool's\n")
        );
        assert!(root
            .join("Cargo.lock")
            .symlink_metadata()
            .unwrap()
            .is_file());
        assert_eq!(
            read(&root.join("projects/web-app/Cargo.lock")).as_deref(),
            Some("# edited\n")
        );
        let issues = verify_surfacing(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
        );
        let conflict = issues
            .iter()
            .find(|i| i.message.contains("neither version control nor"))
            .unwrap_or_else(|| panic!("the conflict must be a finding: {issues:?}"));
        assert!(
            !conflict.safe_to_fix,
            "neither side may be chosen for the operator"
        );
        assert!(
            conflict.message.contains("projects/web-app/Cargo.lock"),
            "both paths are named: {}",
            conflict.message
        );
    }

    #[test]
    fn a_verb_that_may_not_write_leaves_a_root_write_and_its_link_out() {
        // `rwv activate` of the project the root already presents.
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        write_root_lock(root, "version = 4\n");

        surface(root, &project, SurfacingMode::Select)
            .expect("an unadopted lock is reported, not refused");

        assert!(root
            .join("Cargo.lock")
            .symlink_metadata()
            .unwrap()
            .is_file());
        assert!(!root.join("projects/web-app/Cargo.lock").exists());
        let issues = verify_surfacing(
            project_vcs().as_ref(),
            root,
            &project,
            &load_manifest(root, &project),
        );
        assert!(
            issues
                .iter()
                .any(|i| i.safe_to_fix && i.message.contains("not yet taken into")),
            "the check names it as something --fix adopts: {issues:?}"
        );
    }

    #[test]
    fn a_directory_where_a_lock_link_belongs_still_refuses() {
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        std::fs::create_dir(root.join("Cargo.lock")).unwrap();

        let err = surface(root, &project, SurfacingMode::Repair).unwrap_err();

        assert_eq!(
            crate::refusal::kind_of(&err),
            Some(RefusalKind::SurfacingPathOccupied),
            "{err:#}"
        );
    }

    /// A second project with a cargo member, its lock written, for switching to.
    fn add_cargo_project(root: &Path, name: &str, lock: &str) -> ProjectName {
        let dir = root.join("projects").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(Manifest::FILE_NAME),
            std::fs::read_to_string(root.join("projects/web-app").join(Manifest::FILE_NAME))
                .unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("Cargo.lock"), lock).unwrap();
        ProjectName::new(name).unwrap()
    }

    #[test]
    fn switching_projects_takes_the_outgoing_root_write_home_first() {
        let (tmp, _web) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        commit_project_lock(root, "web-app", "# web\n");
        write_root_lock(root, "# web, written past its link\n");
        let api = add_cargo_project(root, "api", "# api\n");

        surface(root, &api, SurfacingMode::Select).unwrap();

        assert_eq!(
            read(&root.join("projects/web-app/Cargo.lock")).as_deref(),
            Some("# web, written past its link\n"),
            "the outgoing project's tool write must not be stranded or discarded"
        );
        assert!(is_lock_link(root, "api"));
    }

    /// Every entry directly under `root`, with what a link reads and what a
    /// file holds, and both projects' locks.
    fn snapshot(root: &Path) -> Vec<(String, String)> {
        let mut entries: Vec<(String, String)> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let state = match std::fs::read_link(&path) {
                    Ok(target) => format!("link {}", target.display()),
                    Err(_) => format!("{:?}", std::fs::read_to_string(&path).ok()),
                };
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    state,
                )
            })
            .collect();
        for project in ["web-app", "api"] {
            entries.push((
                format!("{project}/Cargo.lock"),
                format!(
                    "{:?}",
                    read(&root.join("projects").join(project).join("Cargo.lock"))
                ),
            ));
        }
        entries.sort();
        entries
    }

    fn conflicting_root_write_in_web_app() -> (tempfile::TempDir, ProjectName) {
        let (tmp, _web) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        commit_project_lock(root, "web-app", "# web\n");
        write_project_lock(root, "web-app", "# edited\n");
        write_root_lock(root, "# the tool's\n");
        let api = add_cargo_project(root, "api", "# api\n");
        (tmp, api)
    }

    #[test]
    fn switching_away_from_a_conflicting_root_write_is_refused_and_moves_nothing() {
        let (tmp, api) = conflicting_root_write_in_web_app();
        let root = tmp.path();
        let before = snapshot(root);

        let err = surface(root, &api, SurfacingMode::Select).unwrap_err();

        assert_eq!(
            crate::refusal::kind_of(&err),
            Some(RefusalKind::ConflictingRootWrite),
            "{err:#}"
        );
        let message = format!("{err:#}");
        for named in [
            "Cargo.lock",
            "projects/web-app/Cargo.lock",
            "rwv activate api",
        ] {
            assert!(message.contains(named), "{named} unnamed in: {message}");
        }
        assert_eq!(snapshot(root), before);
    }

    #[test]
    fn a_switch_refused_over_a_conflict_proceeds_once_it_is_resolved() {
        let (tmp, api) = conflicting_root_write_in_web_app();
        let root = tmp.path();
        surface(root, &api, SurfacingMode::Select).unwrap_err();

        std::fs::remove_file(root.join("Cargo.lock")).unwrap();
        surface(root, &api, SurfacingMode::Select).unwrap();
        std::fs::write(root.join(".rwv-active"), "api\n").unwrap();
        surface(root, &api, SurfacingMode::Repair).unwrap();

        assert!(is_lock_link(root, "api"));
        assert_eq!(
            read(&root.join("projects/api/Cargo.lock")).as_deref(),
            Some("# api\n"),
            "no pass after the switch takes anything into the incoming project"
        );
        assert_eq!(
            read(&root.join("projects/web-app/Cargo.lock")).as_deref(),
            Some("# edited\n")
        );
    }

    #[test]
    fn a_root_write_whose_verdict_errs_is_reported_as_an_error_not_as_adoptable() {
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        write_project_lock(root, "web-app", "# project's\n");
        write_root_lock(root, "# the tool's\n");
        let vcs = crate::vcs::testing::FakeVcs::new();
        vcs.fail_next(
            crate::vcs::testing::VcsCall::HoldsCommittedContent,
            crate::vcs::VcsError::NotARepo(root.join("projects/web-app")),
        );

        let issues = verify_surfacing(&vcs, root, &project, &load_manifest(root, &project));

        let issue = issues
            .iter()
            .find(|issue| issue.message.contains("Cargo.lock"))
            .unwrap_or_else(|| panic!("the root write must be reported: {issues:?}"));
        assert_eq!(
            issue.severity,
            crate::integration::Severity::Error,
            "{issue:?}"
        );
        assert!(!issue.safe_to_fix, "{issue:?}");
    }

    #[test]
    fn a_root_write_that_cannot_be_read_is_reported() {
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        write_root_lock(root, "# the tool's\n");
        std::fs::create_dir(root.join("projects/web-app/Cargo.lock")).unwrap();
        let manifest = load_manifest(root, &project);

        let notices = root_write_notices(root, &project, &manifest);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("could not be read"), "{notices:?}");

        let issues = verify_surfacing(project_vcs().as_ref(), root, &project, &manifest);
        let issue = issues
            .iter()
            .find(|issue| issue.message.contains("could not be determined"))
            .unwrap_or_else(|| panic!("the read error must be reported: {issues:?}"));
        assert_eq!(
            issue.severity,
            crate::integration::Severity::Error,
            "{issue:?}"
        );
        assert!(!issue.safe_to_fix, "{issue:?}");
    }

    #[test]
    #[cfg(unix)]
    fn a_failure_after_an_adoption_puts_both_files_back() {
        // The lock is adopted, then the link under the read-only `sub/` cannot
        // be made.
        let (tmp, project) = make_cargo_surfacing_workspace();
        let root = tmp.path();
        commit_project_lock(root, "web-app", "# committed\n");
        write_root_lock(root, "# regenerated\n");
        let manifest_path = root.join("projects/web-app").join(Manifest::FILE_NAME);
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        std::fs::write(
            &manifest_path,
            text.replace(
                "[integrations.vscode-workspace]",
                "[integrations.static-files]\nenabled = true\nfiles = [\"sub/notes.md\"]\n\n\
                 [integrations.vscode-workspace]",
            ),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("projects/web-app/sub")).unwrap();
        std::fs::write(root.join("projects/web-app/sub/notes.md"), "n").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        let writable = read_only(&root.join("sub"));

        let result = surface(root, &project, SurfacingMode::Repair);
        std::fs::set_permissions(root.join("sub"), writable).unwrap();

        assert!(result.is_err(), "the obstruction must stop the pass");
        assert_eq!(
            read(&root.join("Cargo.lock")).as_deref(),
            Some("# regenerated\n"),
            "the root write the pass had taken in must be put back"
        );
        assert!(root
            .join("Cargo.lock")
            .symlink_metadata()
            .unwrap()
            .is_file());
        assert_eq!(
            read(&root.join("projects/web-app/Cargo.lock")).as_deref(),
            Some("# committed\n"),
            "and the project copy it had overwritten"
        );
    }

    #[test]
    fn verify_flags_stale_symlink_when_source_missing() {
        // The incident this pins: a symlink surfaced while the source existed
        // (or left by a repair scoped elsewhere), and the source is gone now.
        // Nothing writes a static file through its link, so the link standing
        // over an absent source is the finding, whether or not it still
        // resolves to the expected target.
        let (tmp, project) = make_surfacing_workspace_authoring(&[".claude"], &[]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        crate::symlink::create(
            Path::new("projects/web-app/.claude"),
            &root.join(".claude"),
            LinkTarget::File,
        )
        .unwrap();

        let issues = verify_surfacing(project_vcs().as_ref(), root, &project, &manifest);
        assert_eq!(
            issues.len(),
            1,
            "expected one stale-symlink issue: {issues:?}"
        );
        assert!(issues[0].safe_to_fix);
        assert!(
            issues[0].message.contains(".claude") && issues[0].message.contains("no longer exists"),
            "message should name the file and say the source is gone: {}",
            issues[0].message
        );
    }

    #[test]
    fn fix_removes_stale_symlink_without_recreating_it() {
        // End-to-end of the --fix primitive for the same state: the stale
        // link is removed, and (unlike the missing-symlink case) nothing
        // replaces it, because the source is still absent.
        let (tmp, project) = make_surfacing_workspace_authoring(&[".claude"], &[]);
        let root = tmp.path();
        let manifest = load_manifest(root, &project);

        crate::symlink::create(
            Path::new("projects/web-app/.claude"),
            &root.join(".claude"),
            LinkTarget::File,
        )
        .unwrap();
        assert_eq!(
            verify_surfacing(project_vcs().as_ref(), root, &project, &manifest).len(),
            1
        );

        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &project,
            &manifest,
            SurfacingMode::Repair,
        )
        .unwrap();

        assert!(
            root.join(".claude").symlink_metadata().is_err(),
            "the stale symlink should be removed, not recreated (source still missing)"
        );
        assert!(
            verify_surfacing(project_vcs().as_ref(), root, &project, &manifest).is_empty(),
            "after --fix, doctor should report nothing"
        );
    }

    // -----------------------------------------------------------------------
    // Two-class surfacing: shared names follow the project the root presents
    // -----------------------------------------------------------------------

    #[test]
    fn project_named_is_the_project_name_plus_an_extension() {
        let web = ProjectName::new("web-app").unwrap();
        assert!(is_project_named("web-app.code-workspace", &web));
        // Shared names: another project produces the same string.
        assert!(!is_project_named("Cargo.toml", &web));
        assert!(!is_project_named(".claude", &web));
        assert!(!is_project_named("gita/repos.csv", &web));
        // A prefix match is not enough — `web-app-notes` is a different name,
        // not `web-app` with an extension.
        assert!(!is_project_named("web-app-notes", &web));
        // Multi-segment project names surface nested, and the whole path is
        // what carries the name.
        let nested = ProjectName::new("acme/web-app").unwrap();
        assert!(is_project_named("acme/web-app.code-workspace", &nested));
        assert!(!is_project_named("web-app.code-workspace", &nested));
    }

    /// Fixture for the two-class rule: the root presents `web-app`, which
    /// declares two shared names; `other-app` declares one of them plus one
    /// named for itself. The other shared name — declared only by the
    /// presented project — is the one a repair scoped to `other-app` can
    /// reach only through the previously-active union.
    fn make_two_project_workspace() -> (tempfile::TempDir, ProjectName, ProjectName) {
        let (tmp, presented) = make_surfacing_workspace(&[".claude", "AGENTS.md"]);
        let other = write_surfacing_project(
            tmp.path(),
            "other-app",
            &[".claude", "other-app.code-workspace"],
            &[".claude", "other-app.code-workspace"],
        );
        let manifest = load_manifest(tmp.path(), &presented);
        surface_symlinks(
            project_vcs().as_ref(),
            tmp.path(),
            &presented,
            &manifest,
            SurfacingMode::Repair,
        )
        .unwrap();
        (tmp, presented, other)
    }

    #[test]
    fn repair_for_another_project_leaves_the_shared_name_with_the_presented_one() {
        // The pinned incident: a repair scoped to a project the weave root
        // does not present re-pointed the root's shared names at it. The
        // per-project-named half of the same repair must still happen.
        let (tmp, presented, other) = make_two_project_workspace();
        let root = tmp.path();

        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &other,
            &load_manifest(root, &other),
            SurfacingMode::Repair,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_link(root.join(".claude")).unwrap(),
            Path::new("projects/web-app/.claude"),
            "a shared name must stay with the project the root presents ({presented})"
        );
        assert_eq!(
            std::fs::read_link(root.join("other-app.code-workspace")).unwrap(),
            Path::new("projects/other-app/other-app.code-workspace"),
            "a per-project name is safe to surface for any project"
        );
    }

    #[test]
    fn repair_for_another_project_does_not_unlink_the_presented_project() {
        // The other half of the same stomp: the removal candidates for a
        // non-presented project must not reach the presented project's
        // surfacing, which the previously-active union made them do. The
        // shared name `other-app` does not declare is the case that isolates
        // removal from re-pointing — a repair that unlinks it never puts
        // anything back.
        let (tmp, _presented, other) = make_two_project_workspace();
        let root = tmp.path();

        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &other,
            &load_manifest(root, &other),
            SurfacingMode::Repair,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_link(root.join("AGENTS.md")).unwrap(),
            Path::new("projects/web-app/AGENTS.md"),
            "the presented project's surfacing must survive another project's repair"
        );
    }

    #[test]
    fn verify_for_another_project_expects_only_its_per_project_names() {
        // The detector half: expecting a shared name to be surfaced for a
        // project the root does not present is what licensed the repair.
        let (tmp, _presented, other) = make_two_project_workspace();
        let root = tmp.path();

        let issues = verify_surfacing(
            project_vcs().as_ref(),
            root,
            &other,
            &load_manifest(root, &other),
        );
        assert_eq!(
            issues.len(),
            1,
            "expected only the per-project name: {issues:?}"
        );
        assert!(
            issues[0].message.contains("other-app.code-workspace"),
            "the one finding must be the per-project name: {}",
            issues[0].message
        );
    }

    #[test]
    fn verify_flags_a_shared_name_surfaced_out_of_another_project() {
        // Residue of the incident: a shared name the presented project does
        // not declare, left pointing into another project. Nothing the
        // presented project declares reaches it, so only a root scan sees it.
        let (tmp, presented, _other) = make_two_project_workspace();
        let root = tmp.path();
        crate::symlink::create(
            Path::new("projects/other-app/Cargo.toml"),
            &root.join("Cargo.toml"),
            LinkTarget::File,
        )
        .unwrap();

        let issues = verify_surfacing(
            project_vcs().as_ref(),
            root,
            &presented,
            &load_manifest(root, &presented),
        );
        assert_eq!(
            issues.len(),
            1,
            "expected the foreign shared name: {issues:?}"
        );
        assert!(issues[0].safe_to_fix);
        assert!(
            issues[0].message.contains("Cargo.toml") && issues[0].message.contains("other-app"),
            "the finding must name the path and the project it resolves into: {}",
            issues[0].message
        );
    }

    #[test]
    fn verify_allows_another_projects_per_project_name_at_the_root() {
        // The counterpart: `other-app.code-workspace` resolving into
        // `other-app` while `web-app` is presented is the intended state, not
        // a finding — that is what makes the two classes worth separating.
        let (tmp, presented, other) = make_two_project_workspace();
        let root = tmp.path();
        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &other,
            &load_manifest(root, &other),
            SurfacingMode::Repair,
        )
        .unwrap();

        let issues = verify_surfacing(
            project_vcs().as_ref(),
            root,
            &presented,
            &load_manifest(root, &presented),
        );
        assert!(
            issues.is_empty(),
            "expected clean surfacing, got: {issues:?}"
        );
    }

    #[test]
    fn re_surfacing_the_presented_project_reclaims_a_foreign_shared_name() {
        let (tmp, presented, _other) = make_two_project_workspace();
        let root = tmp.path();
        crate::symlink::create(
            Path::new("projects/other-app/Cargo.toml"),
            &root.join("Cargo.toml"),
            LinkTarget::File,
        )
        .unwrap();

        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &presented,
            &load_manifest(root, &presented),
            SurfacingMode::Repair,
        )
        .unwrap();

        assert!(
            root.join("Cargo.toml").symlink_metadata().is_err(),
            "re-surfacing the presented project must clear a foreign shared name"
        );
    }

    #[test]
    fn repair_refuses_a_per_project_name_the_presented_project_also_claims() {
        // The class rule reads a project name out of a path, so a project
        // named `go` makes `go.work` look per-project-named while it is also
        // the shared name the presented project surfaces. Refuse: taking it
        // would be the stomp, and skipping it would hide a naming collision
        // rwv cannot resolve.
        let (tmp, _presented, _other) = make_two_project_workspace();
        let root = tmp.path();
        let presented = write_surfacing_project(root, "web-app", &["go.work"], &["go.work"]);
        let go = write_surfacing_project(root, "go", &["go.work"], &["go.work"]);
        surface_symlinks(
            project_vcs().as_ref(),
            root,
            &presented,
            &load_manifest(root, &presented),
            SurfacingMode::Repair,
        )
        .unwrap();

        let err = surface_symlinks(
            project_vcs().as_ref(),
            root,
            &go,
            &load_manifest(root, &go),
            SurfacingMode::Repair,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("go.work") && msg.contains("web-app"),
            "the refusal must name the path and the project holding it: {msg}"
        );
        assert_eq!(
            std::fs::read_link(root.join("go.work")).unwrap(),
            Path::new("projects/web-app/go.work"),
            "the refusal must leave the presented project's surfacing in place"
        );
    }

    // -----------------------------------------------------------------------
    // Arriving at drift in an attested generated file
    // -----------------------------------------------------------------------

    mod arrived_drift {
        use super::*;
        use crate::cli::consent::{AdoptDriftedConsent, RegenerateDriftedConsent};
        use crate::owned_state::{check_owned_digest, OwnedDigestCheck};

        /// A directory where `name` was accepted holding `accepted`, and now
        /// holds `on_disk`.
        fn attested(name: &str, accepted: &[u8], on_disk: &[u8]) -> tempfile::TempDir {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path();
            std::fs::write(dir.join(name), accepted).unwrap();
            stamp_owned_digest(dir, name, accepted).unwrap();
            std::fs::write(dir.join(name), on_disk).unwrap();
            tmp
        }

        fn regenerate() -> Option<DriftConsent> {
            Some(DriftConsent::Regenerate(RegenerateDriftedConsent::granted()))
        }

        fn adopt() -> Option<DriftConsent> {
            Some(DriftConsent::Adopt(AdoptDriftedConsent::granted()))
        }

        #[test]
        fn content_rwv_accepted_is_not_a_fork() {
            let tmp = attested("Cargo.lock", b"version = 4\n", b"version = 4\n");
            settle_arrived_drift(tmp.path(), None)
                .expect("the common case must not ask the operator anything");
        }

        #[test]
        fn an_attested_file_that_is_gone_is_not_a_fork() {
            let tmp = tempfile::tempdir().unwrap();
            stamp_owned_digest(tmp.path(), "Cargo.lock", b"version = 4\n").unwrap();
            settle_arrived_drift(tmp.path(), None)
                .expect("a file to regenerate is not a choice between two losses");
        }

        #[test]
        fn drift_with_no_consent_refuses_naming_the_path_and_both_exits() {
            let tmp = attested("Cargo.lock", b"version = 4\n", b"version = 3\n");
            let msg = settle_arrived_drift(tmp.path(), None)
                .expect_err("drift with no consent must refuse")
                .to_string();
            assert!(
                msg.contains(&tmp.path().join("Cargo.lock").display().to_string()),
                "the refusal is the loss list the override is read against: {msg}"
            );
            assert!(
                msg.contains("--adopt-drifted") && msg.contains("--regenerate-drifted"),
                "a refusal that does not name both exits strands the operator: {msg}"
            );
            assert_eq!(
                std::fs::read(tmp.path().join("Cargo.lock")).unwrap(),
                b"version = 3\n",
                "refusing must leave the file alone"
            );
        }

        #[test]
        fn adopt_attests_the_bytes_the_check_compared() {
            let tmp = attested("Cargo.lock", b"version = 4\n", b"version = 3\n");
            settle_arrived_drift(tmp.path(), adopt()).unwrap();
            assert_eq!(
                std::fs::read(tmp.path().join("Cargo.lock")).unwrap(),
                b"version = 3\n",
                "adopting must not rewrite the content it accepts"
            );
            assert_eq!(
                check_owned_digest(tmp.path(), "Cargo.lock", b"version = 3\n"),
                OwnedDigestCheck::Matches,
                "the ledger must now attest what is on disk"
            );
        }

        #[test]
        fn regenerate_discards_the_content_so_the_generator_makes_it_again() {
            let tmp = attested("Cargo.lock", b"version = 4\n", b"version = 3\n");
            settle_arrived_drift(tmp.path(), regenerate()).unwrap();
            assert!(
                !tmp.path().join("Cargo.lock").exists(),
                "the hooks that follow create what is absent; content left in \
                 place is content a generator may decline to replace"
            );
        }
    }

    fn workweave_sites(primary: PrimaryProjects) -> SurfacingSites {
        SurfacingSites {
            primary,
            workweave_links: BTreeSet::new(),
        }
    }

    fn written_through(copy: SurfacedCopy) -> SurfacedEntry {
        SurfacedEntry {
            integration: "test".into(),
            source: SurfacedSource::WrittenThroughLink,
            copy,
        }
    }

    #[test]
    fn a_root_write_is_adopted_into_the_weaves_own_copy_only() {
        let sites = workweave_sites(PrimaryProjects::At("/primary/projects".into()));
        let project = ProjectName::new("alpha").unwrap();
        for (copy, adopts) in [(SurfacedCopy::Weave, true), (SurfacedCopy::Primary, false)] {
            let entry = written_through(copy);
            let planned = sites
                .link(Path::new("/ww"), &project, "x.lock", entry.copy)
                .unwrap();
            assert_eq!(planned.adopts_root_writes(&entry), adopts, "{copy:?}");
        }
    }

    /// Only Windows spells one path two ways, so a `/verbatim` prefix stands in
    /// for `\\?\` here: what matters is that the plan is spelled the way the
    /// link reads back, and that the read-back is recognized as rwv's.
    #[test]
    fn a_primary_sourced_link_is_planned_in_the_spelling_it_reads_back_in() {
        fn as_read_back(path: &Path) -> &Path {
            path.to_str()
                .and_then(|s| s.strip_prefix("/verbatim"))
                .map_or(path, Path::new)
        }
        let sites = workweave_sites(PrimaryProjects::under(
            Path::new("/verbatim/primary"),
            as_read_back,
        ));
        let project = ProjectName::new("alpha").unwrap();
        let read_back = Path::new("/primary/projects/alpha/shared.db");

        let planned = sites
            .link(
                Path::new("/ww"),
                &project,
                "shared.db",
                SurfacedCopy::Primary,
            )
            .unwrap();
        assert_eq!(planned.target, read_back);
        assert_eq!(
            sites
                .primary_owner_dir(Path::new("shared.db"), read_back)
                .as_deref(),
            Some("alpha"),
            "the link as read back must be recognized as rwv's own surfacing"
        );
    }

    #[test]
    fn nothing_is_linked_from_a_primary_the_marker_cannot_witness() {
        let sites = workweave_sites(PrimaryProjects::Unknown);
        let project = ProjectName::new("alpha").unwrap();
        assert!(sites
            .link(Path::new("/ww"), &project, ".beads", SurfacedCopy::Primary)
            .is_none());
        assert!(sites
            .link(Path::new("/ww"), &project, ".beads", SurfacedCopy::Weave)
            .is_some());
    }

    #[test]
    fn a_second_declaration_coalesces_toward_written_through_and_the_primary() {
        let mut entry = SurfacedEntry {
            integration: "test".into(),
            source: SurfacedSource::WrittenAtSource,
            copy: SurfacedCopy::Primary,
        };
        entry.absorb(SurfacedSource::WrittenThroughLink, SurfacedCopy::Weave);
        assert_eq!(entry.source, SurfacedSource::WrittenThroughLink);
        assert_eq!(entry.copy, SurfacedCopy::Primary);

        let mut entry = written_through(SurfacedCopy::Weave);
        entry.absorb(SurfacedSource::WrittenAtSource, SurfacedCopy::Primary);
        assert_eq!(entry.source, SurfacedSource::WrittenThroughLink);
        assert_eq!(entry.copy, SurfacedCopy::Primary);
    }
}
