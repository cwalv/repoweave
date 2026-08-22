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
use crate::integration::{Integration, OwnedPath, Severity, SurfacedSource};
use crate::integration_runner::{
    build_detection_cache, disabled_integration_artifacts, enabled_integrations,
    run_activate_hooks, run_activations, run_checks, run_deactivations, run_verifications,
};
use crate::integrations::builtin_integrations;
use crate::manifest::{IntegrationConfig, Manifest, ProjectName};
use crate::owned_state::{
    attested_owned_files, drifted_attested_owned_files, forget_owned_digest, ledger_path,
    reset_unreadable_ledger, stamp_owned_digest,
};
use crate::refusal::RefusalKind;
use crate::symlink::LinkTarget;
use crate::workspace::{
    observe_root, project_dir, project_rel_dir, strip_projects_prefix, workspace_marker_names,
    RootObservation, WorkspaceContext, WorkspaceSession,
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
/// **Install hooks run**, unlike the workweave-flavoured
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
    let project_name = ProjectName::new(project)?;
    let project_dir = project_dir(root, project);
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let manifest = Manifest::from_path(&manifest_path)?;

    // Discover repos on disk and project paths (needed by IntegrationContext).
    let session = WorkspaceSession::new(root);

    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();

    // Integration content step.
    let detection_cache = build_detection_cache(&integrations, root, manifest.iter_entries());
    let ctx_base =
        session.context_base(&project_name, &detection_cache, manifest.workweave.as_ref());

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
            //   - run_verifications: drift between intent (rwv.toml/.lock)
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
        ActivationMode::Materialize(consent) => {
            strip_disabled_integrations(root, &integrations, &manifest, &ctx_base)?;
            forget_unproduced_attestations(&integrations, &manifest, &ctx_base)?;
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
            != Some(&project_name)
        {
            return Ok(());
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
    surface_symlinks(root, &project_name, &manifest, surfacing_mode)?;

    // Run integration activate hooks (install commands).
    //    Per-integration hooks operate on the now-in-place symlinks at the
    //    workspace root (e.g., `npm install` reads the symlinked
    //    package.json). Suppressed by `--no-materialize` for fast
    //    context-switches; the user can run install commands directly when
    //    they need them.
    if !opts.no_materialize && !withhold_hooks_over_unsettled_drift(&ctx_base.output_dir) {
        let hook_issues = run_activate_hooks(&integrations, &manifest, &ctx_base);
        report_and_check_activate_hook_issues(&hook_issues)?;
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
/// Directories that were created solely to hold nested symlinks are cleaned
/// up if they become empty.
fn remove_activation_symlinks(root: &Path, owned_files: &BTreeSet<String>) -> anyhow::Result<()> {
    remove_activation_symlinks_in(root, root, owned_files)
}

/// Which of `names` the weave root currently surfaces out of a project, in
/// `names` order. Read-only.
///
/// Owner-scoped by the same predicate the removal path uses, so a root entry
/// that is a real file, a dangling name, or a link pointing somewhere else is
/// not counted as surfacing.
pub fn surfaced_names(root: &Path, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            std::fs::read_link(root.join(name))
                .is_ok_and(|target| target_resolves_to_projects(Path::new(name), &target))
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
    remove_activation_symlinks(root, &names.iter().cloned().collect())
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
fn target_resolves_to_projects(rel_from_root: &Path, target: &Path) -> bool {
    surfacing_owner_dir(rel_from_root, target).is_some()
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
fn surfacing_owner_dir(rel_from_root: &Path, target: &Path) -> Option<String> {
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
    let under_projects = strip_projects_prefix(&sited)?;
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
                let resolves_to_projects = target_resolves_to_projects(rel_from_root, &target);
                if in_owned_set && resolves_to_projects {
                    crate::symlink::remove(&path)?;
                }
            }
        } else if meta.file_type().is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if workspace_marker_names().iter().any(|m| m == name)
                    || name == crate::git::GIT_DIR_ENTRY_NAME
                {
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
    activate_at(
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
/// is coalesced (the set is keyed by path).
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
            let (name, source) = f.into_parts();
            owned
                .entry(name)
                .and_modify(|held| {
                    if source == SurfacedSource::WrittenThroughLink {
                        held.source = source;
                    }
                })
                .or_insert_with(|| SurfacedEntry {
                    integration: integration.name().to_string(),
                    source,
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

/// Compute the owner-scoped surfacing set for the project `root` currently
/// presents — the pointer at primary, the marker in a workweave. Returns an
/// empty set if `root` presents no project (in which case no symlinks are
/// owned by rwv and nothing gets removed). This is the deactivate-side
/// analogue of step 2 in [`activate_at`].
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
///     `projects/<project>/<file>`.
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
pub fn surface_symlinks(
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    mode: SurfacingMode,
) -> anyhow::Result<()> {
    let project_dir = project_dir(root, project.as_str());
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
    let new_generated: Vec<(&String, &SurfacedEntry)> = owned
        .iter()
        .filter(|(file, _)| new_owned.contains(file.as_str()))
        .collect();

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
    let removal_candidates = if presents_project {
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
    remove_activation_symlinks(root, &removal_candidates)?;

    // 2b. Shared names surfaced out of some other project — left behind by a
    //     project switch, or taken by a repair that predates the rule. The
    //     presented project's declarations never reach them, so the removal
    //     above cannot.
    if presents_project {
        for (file, _owner) in foreign_shared_name_links(root, project, &new_owned) {
            crate::symlink::remove(&root.join(&file))?;
        }
    }

    // 3. Create new symlinks at root pointing to project_dir files.
    for (file, entry) in &new_generated {
        let source = project_dir.join(file);
        let link = root.join(file);

        if link_waits_for_its_source(entry, &source) {
            continue;
        }

        // Ensure parent directory exists for nested files (e.g., gita/repos.csv).
        if let Some(parent) = link.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "surfacing `{file}`: failed to create parent directory {}",
                        parent.display()
                    )
                })?;
            }
        }

        // Compute a relative symlink target from the link location to the
        // source in the project directory. For top-level files this is just
        // `projects/{project}/{file}`. For nested files like `gita/repos.csv`
        // we need to prepend `../` for each directory level.
        let relative_target = relative_symlink_target(project.as_str(), file);

        crate::symlink::create(&relative_target, &link, LinkTarget::on_disk(&source))?;
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
    relative_target.push(project_rel_dir(project));
    relative_target.push(file);
    relative_target
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
    let mut found = Vec::new();
    collect_undeclared_links_in(root, root, presented, &exempt, &mut found);
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
    presented: &ProjectName,
    declared: &BTreeSet<String>,
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
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if workspace_marker_names().iter().any(|m| m == name)
                    || name == crate::git::GIT_DIR_ENTRY_NAME
                {
                    continue;
                }
            }
            collect_undeclared_links_in(&path, root, presented, declared, found);
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
        let Some(owner) = surfacing_owner_dir(rel_from_root, &target) else {
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
/// resolves to `projects/<project>/<file>`.
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
    root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> Vec<crate::integration::Issue> {
    use crate::integration::{Issue, IssueKind, Severity};

    let project_dir = project_dir(root, project.as_str());
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
        let source = project_dir.join(file);
        let source_missing = link_waits_for_its_source(entry, &source);
        let integration = &entry.integration;

        let link = root.join(file);
        let expected_target = relative_symlink_target(project.as_str(), file);

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
            Ok(actual) if actual == expected_target => {
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
                issues.push(Issue {
                    integration: integration.clone(),
                    severity: Severity::Warning,
                    message: format!(
                        "surfacing: `{file}` symlink resolves to `{}` (expected `{}`; safe to --fix)",
                        actual.display(),
                        expected_target.display()
                    ),
                    kind: IssueKind::Surfacing,
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

        let err = surface_symlinks(root, &project, &manifest, SurfacingMode::Repair).unwrap_err();
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

        surface_symlinks(root, &project, &manifest, SurfacingMode::Repair).unwrap();

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

        surface_symlinks(root, &project, &manifest, SurfacingMode::Repair).unwrap();

        // The symlink exists at the root and resolves to the project copy.
        let link = root.join(".claude");
        let meta = link.symlink_metadata().unwrap();
        assert!(meta.file_type().is_symlink(), ".claude should be a symlink");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("projects/web-app/.claude")
        );

        // verify_surfacing reports nothing — surfacing matches the union.
        let issues = verify_surfacing(root, &project, &manifest);
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
        let issues = verify_surfacing(root, &project, &manifest);
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

        let issues = verify_surfacing(root, &project, &manifest);
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

        let issues = verify_surfacing(root, &project, &manifest);
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
        assert_eq!(verify_surfacing(root, &project, &manifest).len(), 1);

        // The fix primitive (NOT activate_intent) creates the symlink.
        surface_symlinks(root, &project, &manifest, SurfacingMode::Repair).unwrap();
        assert!(
            verify_surfacing(root, &project, &manifest).is_empty(),
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

        surface_symlinks(root, &other, &manifest, SurfacingMode::Select).unwrap();
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

        assert!(verify_surfacing(root, &project, &manifest).is_empty());
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

        surface_symlinks(root, &project, &manifest, SurfacingMode::Repair).unwrap();

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

        let issues = verify_surfacing(root, &project, &manifest);
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
            surface_symlinks(root, &project, &manifest, SurfacingMode::Repair).unwrap();
            assert!(
                root.join("Cargo.lock").symlink_metadata().is_ok(),
                "round {round}: the pending link was removed"
            );
            let issues = verify_surfacing(root, &project, &manifest);
            assert!(
                issues.is_empty(),
                "round {round}: a repair left something for the next check to \
                 report, which is the loop: {issues:?}"
            );
        }
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

        let issues = verify_surfacing(root, &project, &manifest);
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
        assert_eq!(verify_surfacing(root, &project, &manifest).len(), 1);

        surface_symlinks(root, &project, &manifest, SurfacingMode::Repair).unwrap();

        assert!(
            root.join(".claude").symlink_metadata().is_err(),
            "the stale symlink should be removed, not recreated (source still missing)"
        );
        assert!(
            verify_surfacing(root, &project, &manifest).is_empty(),
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
        surface_symlinks(tmp.path(), &presented, &manifest, SurfacingMode::Repair).unwrap();
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

        let issues = verify_surfacing(root, &other, &load_manifest(root, &other));
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

        let issues = verify_surfacing(root, &presented, &load_manifest(root, &presented));
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
            root,
            &other,
            &load_manifest(root, &other),
            SurfacingMode::Repair,
        )
        .unwrap();

        let issues = verify_surfacing(root, &presented, &load_manifest(root, &presented));
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
            root,
            &presented,
            &load_manifest(root, &presented),
            SurfacingMode::Repair,
        )
        .unwrap();

        let err = surface_symlinks(root, &go, &load_manifest(root, &go), SurfacingMode::Repair)
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
}
