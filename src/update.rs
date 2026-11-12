//! `rwv update` — clone the members missing on disk, align the rest to the
//! checked-out branch's record, and report what the lock leaves unpinned.
//!
//! The engine here is also what `rwv clone` runs once it has the project
//! repo: a clone is an update of a weave with nothing in it.

use crate::activate::ActivateOptions;
use crate::alignment::{Aligner, Applied, Member, ProjectStanding};
use crate::diagnosis::JudgedOutput;
use crate::manifest::{LockEntry, LockFile, Manifest, RepoEntry, RepoPath};
use crate::member_selection::MemberSelection;
use crate::parallel::{run_in_parallel, OutputMode, Reporter};
use crate::placement::PlacementOutput;
use crate::refusal::RefusalKind;
use crate::registry;
use crate::skipped::{SkipReason, SkippedEffect};
use crate::vcs::{vcs_for, PinOutcome, RawRevisionId, Vcs};
use crate::workspace::{
    observe_root, project_dir, Checkout, Resolution, RootObservation, WorkspaceContext,
};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Schema URL embedded in `rwv update --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/update.json`.
pub const UPDATE_SCHEMA_URL: &str = crate::schema_url::schema_url!("update");

/// Per-member outcome record for `rwv update --json` and `rwv clone --json`.
///
/// `status` is one of `"ok"`, `"skipped"`, or `"failed"`. `message` carries
/// a human-readable description of the outcome (always present for `"failed"`;
/// present for `"skipped"` to say why; `null` for `"ok"`).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemberOutcomeOutput {
    pub path: String,
    pub absolute_path: crate::path_spelling::WirePath,
    pub status: MemberOutcomeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The refusal token of a `failed` repo, resolvable with `rwv explain
    /// <token>`. Absent when the failure carries no classified refusal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// The branch the checkout was moved off, when it was neither a declared
    /// branch nor named after a recorded weave branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
    /// What holds for the member in its expected state, such as the branch
    /// it would sit on being checked out in another workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<crate::diagnosis::FactOutput>,
}

/// Status discriminant for [`MemberOutcomeOutput`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MemberOutcomeStatus {
    Ok,
    Skipped,
    Failed,
}

/// What a run of [`bring_members`] reports about the members, before it is
/// given the envelope of the verb that ran it.
pub(crate) struct MembersReport {
    outcomes: Vec<MemberOutcomeOutput>,
    judged: Vec<JudgedOutput>,
    placements: Vec<PlacementOutput>,
    resolution: Option<Resolution>,
    pub(crate) skipped: Vec<SkippedEffect>,
}

/// Top-level envelope for `rwv update --json`.
///
/// Shape: `{ "$schema": "<url>", "outcomes": [<MemberOutcomeOutput>, ...] }`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub outcomes: Vec<MemberOutcomeOutput>,
    /// Each member the update left where it was, judged on the `alignment`
    /// axis (and `edits`, where uncommitted changes kept it), with that
    /// state's choices. Empty, not absent, when it left none.
    pub judged: Vec<JudgedOutput>,
    /// How the files of each checkout the update moved or created were placed:
    /// a canonical clone it realigned, and from a workweave each worktree it
    /// created there. Empty, not absent, when it placed none.
    pub placements: Vec<PlacementOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// The materialization side effects this update skipped, each naming the
    /// command that performs it. Empty, not absent, when it skipped none.
    #[serde(default)]
    pub skipped: Vec<SkippedEffect>,
}

impl MembersReport {
    pub(crate) fn into_update_envelope(self) -> UpdateJsonOutput {
        UpdateJsonOutput {
            schema: UPDATE_SCHEMA_URL.to_owned(),
            outcomes: self.outcomes,
            judged: self.judged,
            placements: self.placements,
            resolution: self.resolution,
            skipped: self.skipped,
        }
    }

    pub(crate) fn into_clone_envelope(
        self,
        left: Vec<JudgedOutput>,
    ) -> crate::clone::CloneJsonOutput {
        crate::clone::CloneJsonOutput {
            schema: crate::clone::CLONE_SCHEMA_URL.to_owned(),
            outcomes: self.outcomes,
            left,
            placements: self.placements,
            resolution: self.resolution,
            skipped: self.skipped,
        }
    }
}

impl UpdateJsonOutput {
    pub(crate) fn print(&self) -> anyhow::Result<()> {
        let out =
            serde_json::to_string_pretty(self).context("failed to serialize update output")?;
        outln!("{out}");
        Ok(())
    }
}

/// Per-repo outcome of the member loop body. Communicates back to the caller
/// (1) whether the repo succeeded and what became of its pin, and (2) what to
/// print in the error report on failure.
enum MemberStep {
    /// Repo handled successfully, its checkout placed as `pin` says.
    Ok { pin: PinOutcome },
    /// Repo intentionally skipped (e.g. `--no-reference` on a reference
    /// repo). Not counted as failure; not counted as success.
    Skipped,
    /// Repo failed. Its `{:#}` rendering is the aggregated-error-report
    /// message, and a refusal tagged anywhere in its chain names the record's
    /// `refusal`.
    Failed(anyhow::Error),
}

/// Clone the members of the active project that are missing from the
/// current workspace, and align the rest to `rwv.lock`.
///
/// The repair for a dangling reference: it resolves the workspace from CWD,
/// iterates the active project's manifest, and clones any repo whose
/// canonical clone directory is missing, at the commit the lock pins, or at
/// the declared branch's tip where it pins none.
///
/// A clone that is already present is realigned rather than skipped: when the
/// lock covers it, `bring_one` resolves the locked revision in that clone's
/// own object store and `realign_present_clone` advances the checkout to it
/// — fast-forwarding the tracking branch's local counterpart, or refusing
/// unless `detach_checkouts` carries a consent. When the lock has no entry for
/// the repo, or there is no lock, the clone is left alone. The lock is not
/// written.
///
/// Clone-topology (I1): the canonical clone always lives at primary's
/// `<weave>/<repo_path>`, even when this verb is invoked from inside a
/// workweave. It therefore materializes into
/// [`WorkspaceContext::primary_path`] first; from a workweave it then creates
/// the worktree for each member the workweave's `rwv.lock` pins and the
/// workweave lacks, as `rwv doctor --fix` would once the revision is local.
///
/// When the primary has no project selected and the invocation named none,
/// the weave's only project is updated and then activated; with zero or
/// several projects there is nothing to select and the run refuses, naming
/// `--project`.
#[allow(clippy::too_many_arguments)]
pub fn run_update(
    ctx: &WorkspaceContext,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
    activation: ActivateOptions,
) -> anyhow::Result<()> {
    let run = update_members(ctx, selection, jobs, output, activation)?;
    if output.is_json() {
        run.envelope.print()?;
    }
    run.tail
}

/// What [`update_members`] did: the envelope it would print, and how the run
/// ended after it. A run that ends in a refusal after its report is complete
/// still has the report to print first.
pub(crate) struct UpdateRun {
    pub(crate) envelope: UpdateJsonOutput,
    pub(crate) tail: anyhow::Result<()>,
}

/// The verb a run of the alignment engine reports as, and whether it moves a
/// member that holds edits as the VCS carries them.
#[derive(Clone, Copy)]
pub(crate) struct Flavor {
    pub(crate) verb: &'static str,
    pub(crate) carry_edits: bool,
}

impl Flavor {
    pub(crate) const UPDATE: Self = Self {
        verb: "update",
        carry_edits: false,
    };
}

/// What a run for a project that the weave root does not present does about the
/// members that project shares with the one it does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SharedMembers {
    /// A member the run would move refuses the run: the project has no claim on
    /// a checkout the presented project sits on.
    Refuse,
    /// The run aligns them: the project is about to be presented.
    Align,
}

/// [`run_update`] without the printing of its envelope.
pub(crate) fn update_members(
    ctx: &WorkspaceContext,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
    activation: ActivateOptions,
) -> anyhow::Result<UpdateRun> {
    update_members_for(
        ctx,
        selection,
        jobs,
        output,
        activation,
        SharedMembers::Refuse,
        Flavor::UPDATE,
    )
}

/// The alignment `switch` and `branch create` run once the project repo is on
/// its branch: `rwv update`'s, reported under the verb's name, and moving a
/// member that holds edits as the VCS carries them.
pub(crate) fn align_after_switch(ctx: &WorkspaceContext, verb: &'static str) -> anyhow::Result<()> {
    update_members_for(
        ctx,
        &MemberSelection::new(crate::selector::RepoFilter::parse(&[], &[])?, false),
        crate::parallel::resolve_jobs(None),
        OutputMode::Text,
        ActivateOptions::default(),
        SharedMembers::Refuse,
        Flavor {
            verb,
            carry_edits: true,
        },
    )?
    .tail
}

/// The alignment half of `rwv activate --update`: every member of the project
/// `ctx` is bound to, cloned where missing and aligned as `rwv update` aligns
/// them, the ones it shares with the presented project included.
pub(crate) fn align_for_activation(
    ctx: &WorkspaceContext,
    activation: ActivateOptions,
) -> anyhow::Result<()> {
    update_members_for(
        ctx,
        &MemberSelection::new(crate::selector::RepoFilter::parse(&[], &[])?, false),
        crate::parallel::resolve_jobs(None),
        OutputMode::Text,
        activation,
        SharedMembers::Align,
        Flavor::UPDATE,
    )?
    .tail
}

fn update_members_for(
    ctx: &WorkspaceContext,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
    activation: ActivateOptions,
    shared: SharedMembers,
    flavor: Flavor,
) -> anyhow::Result<UpdateRun> {
    let invocation = ctx;
    let sole = sole_unselected_project(invocation);
    let bound;
    let ctx = match &sole {
        Some(project) => {
            bound = WorkspaceContext::resolve_for_project(invocation.active_path(), project)?;
            &bound
        }
        None => invocation,
    };
    let name = ctx.require_active_project_on_disk()?.clone();
    crate::branch::require_recorded(ctx)?;
    let aligner = Aligner::open(
        ctx.active_path(),
        &name,
        crate::alignment::Accounting::scan(ctx.primary_path()),
    )?;
    refuse_detached_project(&aligner, flavor.verb)?;
    let aligner = if flavor.carry_edits {
        aligner.carrying_edits()
    } else {
        aligner
    };
    let aligner = match (&ctx.checkout, shared) {
        (Checkout::Primary { .. }, SharedMembers::Refuse) => {
            crate::shared_members::refuse_moving(ctx.primary_path(), &name, &|path, role| {
                selection.covers(path, role)
            })?;
            aligner
        }
        (Checkout::Primary { .. }, SharedMembers::Align) => {
            match crate::shared_members::outgoing(ctx.primary_path(), &name) {
                Some(outgoing) => aligner.leaving(Aligner::open(
                    ctx.primary_path(),
                    &outgoing,
                    crate::alignment::Accounting::default(),
                )?),
                None => aligner,
            }
        }
        (Checkout::Workweave { .. }, _) => aligner,
    };
    if let Some(why) = &aligner.corrupt {
        eprintln!(
            "rwv {}: the workspace state of {} reads as absent: {why}",
            flavor.verb,
            crate::workspace::project_rel_path(&name)
        );
    }

    // Per-workspace state (rwv.toml, rwv.lock) lives under active_path — the
    // workweave when in one, primary otherwise. Mirrors add_remove.rs's
    // find_project_dir.
    let project_dir = project_dir(ctx.active_path(), &name);

    // Clone destination (canonical store) is always primary's slot — clone-
    // topology I1. Passing primary_path() as workspace_root routes bring_one
    // to write clones at `<primary>/<repo_path>/`, even when CWD is inside a
    // workweave. See docs/explanation/joints/clone-topology.md.
    let workspace_root = ctx.primary_path();
    let primary_lock = match &ctx.checkout {
        Checkout::Workweave { .. } => {
            let path = LockFile::path_in(&crate::workspace::project_dir(workspace_root, &name));
            if path.exists() {
                Some(
                    LockFile::from_state(&path)
                        .with_context(|| format!("failed to read lock file at {path}"))?,
                )
            } else {
                None
            }
        }
        Checkout::Primary { .. } => None,
    };
    let duty = match &ctx.checkout {
        Checkout::Workweave { .. } => CloneDuty::SharedStore {
            primary_lock: primary_lock.as_ref(),
        },
        Checkout::Primary { .. } => CloneDuty::Checkouts,
    };

    let run = match bring_members(
        flavor.verb,
        &project_dir,
        workspace_root,
        selection,
        jobs,
        output,
        Some(ctx.resolution()),
        duty,
    ) {
        Ok(run) => run,
        Err(aborted) => {
            return match aborted.report {
                Some(report) => Ok(UpdateRun {
                    envelope: report.into_update_envelope(),
                    tail: Err(aborted.error),
                }),
                None => Err(aborted.error),
            };
        }
    };
    let moved_or_created = run.moved_or_created;
    let mut report = run.report;

    let created = match &ctx.checkout {
        Checkout::Workweave {
            name: workweave,
            dir,
            project,
            ..
        } => materialize_workweave_members(
            ctx,
            &name,
            workweave.require(dir.as_path(), project)?,
            &project_dir,
            selection,
            output,
            flavor.verb,
            &mut report.placements,
        ),
        Checkout::Primary { .. } => Ok(false),
    };
    let aligned = match &created {
        Ok(_) => align_checkouts(
            ctx,
            &aligner,
            &project_dir,
            selection,
            jobs,
            output,
            flavor.verb,
        ),
        Err(_) => Ok(AlignRun::default()),
    };
    let aligned = match aligned {
        Ok(run) => run,
        Err(error) => {
            return Ok(UpdateRun {
                envelope: report.into_update_envelope(),
                tail: Err(error),
            });
        }
    };
    report.outcomes_merge(&aligned);
    report.judged.extend(aligned.judged.iter().cloned());
    if output.is_json() {
        for (rp, dir, files) in &aligned.placed {
            report
                .placements
                .push(PlacementOutput::new(rp, dir, *files));
        }
    }
    if aligned.failures.is_empty() && created.is_ok() && selection.names_every_member() {
        let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
        let standing =
            crate::entry_report::report("update", ctx, &aligner, &project_dir, &manifest, true);
        crate::alignment::store_state(
            crate::vcs::project_vcs().as_ref(),
            &project_dir,
            &aligner.next_state_storing(&manifest, standing.store),
        )?;
    }
    // In a workweave the only clones update changes are canonical clones it
    // created for the primary; only a worktree it created is this workweave's.
    let checkout_changed = aligned.changed
        || match &ctx.checkout {
            Checkout::Workweave { .. } => matches!(created, Ok(true)),
            Checkout::Primary { .. } => moved_or_created,
        };
    if let Some(project) = &sole {
        let msg = format!(
            "rwv {}: activating project '{project}', the only project in this weave \
             (none was active)",
            flavor.verb
        );
        if output == OutputMode::Text {
            outln!("{msg}");
        } else {
            eprintln!("{msg}");
        }
        report
            .skipped
            .extend(crate::activate::activate_with_options(
                project, invocation, activation,
            )?);
    } else if checkout_changed && shared == SharedMembers::Refuse {
        let presented = observe_root(ctx.active_path())
            .as_ref()
            .and_then(RootObservation::presented_project)
            .cloned();
        let skip = match presented {
            Some(active) if active == name => SkippedEffect::hooks_while_active(&active),
            _ => SkippedEffect::install_hooks(SkipReason::NotPresented {
                project: name.clone(),
            }),
        };
        skip.report();
        report.skipped.push(skip);
    }
    let tail = match created {
        Err(error) => Err(error),
        Ok(_) if !aligned.failures.is_empty() => Err(crate::refusal::refusal(
            RefusalKind::PartialRunAborted,
            format!(
                "update could not align {} repo(s):\n  {}",
                aligned.failures.len(),
                aligned.failures.join("\n  ")
            ),
        )),
        Ok(_) => Ok(()),
    };
    Ok(UpdateRun {
        envelope: report.into_update_envelope(),
        tail,
    })
}

/// The project an update selects for itself: the weave's only one,
/// when the primary has none selected and the invocation named none.
fn sole_unselected_project(ctx: &WorkspaceContext) -> Option<crate::manifest::ProjectName> {
    if !matches!(ctx.checkout, Checkout::Primary { project: None }) {
        return None;
    }
    match crate::workspace::discover_projects(ctx.primary_path()).as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// Give the workweave the worktree of each member it lacks — nothing at the
/// slot while its `rwv.lock` pins the member, a symlink, or a checkout whose
/// link dangles — through [`crate::workweave::materialize_member`], once the
/// canonical clones hold the pinned revisions, returning
/// whether it created any. A symlinked member the lock does not pin starts at
/// the canonical clone's HEAD, which is what the link showed. How each one's
/// files were placed is added to `placements` for the envelope.
#[allow(clippy::too_many_arguments)]
fn materialize_workweave_members(
    ctx: &WorkspaceContext,
    project: &crate::manifest::ProjectName,
    workweave: &crate::manifest::WorkweaveName,
    project_dir: &Path,
    selection: &MemberSelection,
    output: OutputMode,
    verb: &str,
    placements: &mut Vec<PlacementOutput>,
) -> anyhow::Result<bool> {
    let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let lock_path = LockFile::path_in(project_dir);
    let lock = if lock_path.exists() {
        Some(LockFile::from_state(&lock_path)?)
    } else {
        None
    };
    let mut failures: Vec<String> = Vec::new();
    let mut created = false;
    for (repo_path, entry) in manifest.iter_entries() {
        if !selection.covers(repo_path, entry.role) {
            continue;
        }
        let slot = ctx.active_path().join(repo_path.as_path());
        let symlinked = crate::workweave::is_symlinked_member(&slot);
        if !symlinked && !crate::workweave::is_dangling_link(&slot) && slot.exists() {
            if crate::workweave::scan_uninitialized_submodules(&slot).is_empty() {
                continue;
            }
            crate::workweave::populate_member_submodules(
                vcs_for(entry.vcs_type).as_ref(),
                &slot,
                repo_path,
                entry.role,
                entry.recurse_submodules,
                &format!("rwv {verb}: "),
            );
            continue;
        }
        let vcs = vcs_for(entry.vcs_type);
        let start = match lock.as_ref().and_then(|lock| lock.get_entry(repo_path)) {
            Some(pin) => pin.version.as_str().to_owned(),
            None if symlinked => {
                let canonical = ctx.primary_path().join(repo_path.as_path());
                match vcs.head_revision(&canonical) {
                    Ok(head) => head.id().as_str().to_owned(),
                    Err(e) => {
                        failures.push(format!("{repo_path}: {e:#}"));
                        continue;
                    }
                }
            }
            None => continue,
        };
        match crate::workweave::materialize_member(
            vcs.as_ref(),
            ctx.primary_path(),
            ctx.active_path(),
            project,
            workweave,
            repo_path,
            entry.role,
            entry.recurse_submodules,
            &start,
            crate::workweave::BranchPolicy::Restore,
        ) {
            Ok(born) => {
                created = true;
                let line = format!(
                    "rwv {verb}: gave this workweave its worktree for {repo_path}: {}{}",
                    born.summary(),
                    crate::share::files_note(born.files),
                );
                if output == OutputMode::Text {
                    outln!("{line}");
                } else {
                    eprintln!("{line}");
                }
                if let (OutputMode::Json, Some(files)) = (output, born.files) {
                    placements.push(PlacementOutput::new(repo_path, &slot, files));
                }
            }
            Err(e) => failures.push(format!("{repo_path}: {e:#}")),
        }
    }
    if failures.is_empty() {
        return Ok(created);
    }
    crate::refuse!(
        RefusalKind::PartialRunAborted,
        "update brought the pinned revisions into the canonical clones but could not \
         create {} worktree(s) in this workweave:\n  {}",
        failures.len(),
        failures.join("\n  ")
    )
}

/// A run of [`bring_members`] that could not finish, and what it had to say
/// about the members before it stopped. A run that stopped before it looked
/// at any member has no report: its refusal is all there is to print.
pub(crate) struct MembersAborted {
    pub(crate) report: Option<MembersReport>,
    pub(crate) error: anyhow::Error,
}

/// What [`bring_members`] did, beyond what its report holds.
pub(crate) struct MembersRun {
    pub(crate) report: MembersReport,
    /// Some checkout's HEAD is not where it was before this run: a clone it
    /// created, or one it moved. Read whatever the output mode, unlike the
    /// report's `placements`, which also has no record for a fresh clone.
    pub(crate) moved_or_created: bool,
    /// Each member the run covered and what became of its pin, in manifest
    /// order.
    pub(crate) pins: Vec<MemberPin>,
}

/// What became of one member's pin on a run.
pub(crate) struct MemberPin {
    pub(crate) path: RepoPath,
    pub(crate) vcs_type: crate::manifest::VcsType,
    pub(crate) outcome: PinOutcome,
}

/// The per-project member loop shared by `rwv clone` (once it has the project
/// repo) and `rwv update`: clone each member not on disk, align each present
/// one to its lock entry, and report what became of every pin.
///
/// `verb` is the command whose progress lines these are. The lock is read and
/// never written: a member it lacks, or pins at a commit no ref reaches,
/// is [`PinOutcome::None`] or [`PinOutcome::Unreachable`] in the result.
///
/// `resolution` is the resolved workspace coordinate block for the `--json`
/// envelope. `None` when no `WorkspaceContext` is available (a clone runs
/// before a context is resolved).
///
/// Returns the report for the caller to print once it has placed anything
/// else, populated only under `--json`. A run with failures returns it inside
/// [`MembersAborted`] with the refusal, for the caller to print first.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bring_members(
    verb: &'static str,
    project_dir: &Path,
    workspace_root: &Path,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
    resolution: Option<Resolution>,
    duty: CloneDuty<'_>,
) -> Result<MembersRun, Box<MembersAborted>> {
    let setup = || -> anyhow::Result<(Manifest, Option<LockFile>)> {
        let manifest_path = project_dir.join(Manifest::FILE_NAME);
        let manifest = Manifest::from_path(&manifest_path)
            .with_context(|| format!("failed to read manifest from {}", manifest_path.display()))?;
        let lock_path = LockFile::path_in(project_dir);
        let existing_lock = if lock_path.exists() {
            Some(
                LockFile::from_state(&lock_path)
                    .with_context(|| format!("failed to read lock file at {lock_path}"))?,
            )
        } else {
            None
        };
        Ok((manifest, existing_lock))
    };
    let (manifest, existing_lock) = setup().map_err(|error| {
        Box::new(MembersAborted {
            report: None,
            error,
        })
    })?;

    // Warn about orphan lock entries (in lock, not in manifest). Doesn't fail
    // and doesn't touch the clones.
    if let Some(ref lock) = existing_lock {
        for repo_path in lock.iter_repo_paths() {
            if !manifest.contains_repo(repo_path) {
                eprintln!(
                    "rwv {verb}: warning: orphan in lock: {} (lock entry has no manifest entry)",
                    repo_path.as_str()
                );
            }
        }
        for (repo_path, version) in lock.entries_pinned_by_name() {
            eprintln!(
                "rwv {verb}: warning: {}",
                crate::lock::pinned_by_name_warning(repo_path, version)
            );
        }
    }

    // Snapshot the work list. BTreeMap iteration is deterministic, so the
    // resulting Vec preserves the serial loop's ordering (acceptance: -j 1
    // matches the previous behavior exactly).
    //
    // Apply the `--role` / `--repo` filter here so the worker pool only sees
    // selected repos. Empty filter is a no-op (every repo passes). See
    // `src/selector.rs` for the grammar and union semantics.
    let work_items: Vec<(RepoPath, RepoEntry, Box<dyn Vcs>)> = manifest
        .iter_entries()
        .filter(|(rp, entry)| selection.names(rp, entry.role))
        .map(|(rp, e)| (rp.clone(), e.clone(), vcs_for(e.vcs_type)))
        .collect();

    let parallel = jobs > 1;
    let write_lock: Mutex<()> = Mutex::new(());

    let outcomes: Vec<(MemberStep, Option<crate::share::FilePlacement>, bool)> =
        run_in_parallel(&work_items, jobs, |_idx, item| {
            let (repo_path, entry, vcs) = item;
            // A parallel reporter forwards subprocess stdout to stdout, which
            // belongs to the envelope under `--json`.
            let reporter = if parallel && output == OutputMode::Text {
                Reporter::parallel(repo_path.as_str().to_string(), &write_lock)
            } else {
                Reporter::serial()
            };
            let member = workspace_root.join(repo_path.as_path());
            let before = vcs.head_revision(&member).ok();
            let step = bring_one(
                verb,
                vcs.as_ref(),
                repo_path,
                entry,
                workspace_root,
                existing_lock.as_ref(),
                duty,
                selection,
                &reporter,
                output,
            );
            if matches!(step, MemberStep::Ok { .. }) {
                crate::workweave::fetch_member_submodules(
                    vcs.as_ref(),
                    &member,
                    repo_path,
                    entry.role,
                    entry.recurse_submodules,
                    &format!("rwv {verb}: "),
                );
            }
            let (step, files) = match step {
                MemberStep::Ok { .. } => {
                    match crate::settle::settle_moved(
                        vcs.as_ref(),
                        &member,
                        entry.role,
                        before.as_ref(),
                    ) {
                        Ok(settled) => (step, settled.files),
                        Err(e) => (MemberStep::Failed(e.context(repo_path.to_string())), None),
                    }
                }
                other => (other, None),
            };
            let moved_or_created =
                matches!(step, MemberStep::Ok { .. }) && before != vcs.head_revision(&member).ok();

            (step, files, moved_or_created)
        });

    // Aggregate outcomes serially in input order — preserves the existing
    // error-report shape.
    let mut succeeded = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut pins: Vec<MemberPin> = Vec::new();

    // Records follow work_items order whatever order the workers finished in.
    let mut report = MembersReport {
        outcomes: Vec::new(),
        judged: Vec::new(),
        placements: Vec::new(),
        resolution,
        skipped: Vec::new(),
    };

    let moved_or_created = outcomes.iter().any(|(_, _, moved)| *moved);
    for ((step, files, _), (repo_path, entry, _vcs)) in outcomes.iter().zip(work_items.iter()) {
        let member = workspace_root.join(repo_path.as_path());
        let abs_path = crate::path_spelling::WirePath::of(&member);
        match step {
            MemberStep::Ok { pin } => {
                succeeded += 1;
                pins.push(MemberPin {
                    path: repo_path.clone(),
                    vcs_type: entry.vcs_type,
                    outcome: *pin,
                });
                if let (OutputMode::Text, Some(files)) = (output, files) {
                    outln!("rwv {verb}: {repo_path}: files {files}");
                }
                if output.is_json() {
                    report.outcomes.push(MemberOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: MemberOutcomeStatus::Ok,
                        message: None,
                        refusal: None,
                        moved_from: None,
                        facts: Vec::new(),
                    });
                    if let Some(files) = files {
                        report
                            .placements
                            .push(PlacementOutput::new(repo_path, &member, *files));
                    }
                }
            }
            MemberStep::Skipped => {
                if output.is_json() {
                    report.outcomes.push(MemberOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: MemberOutcomeStatus::Skipped,
                        message: Some(format!("skipped {}", repo_path.as_str())),
                        refusal: None,
                        moved_from: None,
                        facts: Vec::new(),
                    });
                }
            }
            MemberStep::Failed(error) => {
                let msg = format!("{error:#}");
                let refusal = crate::refusal::kind_of(error).map(RefusalKind::token);
                if output == OutputMode::Text {
                    eprintln!("rwv {verb}: error: {msg}");
                    for route in crate::refusal::routes(error) {
                        eprintln!("rwv explain {route}");
                    }
                }
                if output.is_json() {
                    report.outcomes.push(MemberOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: MemberOutcomeStatus::Failed,
                        message: Some(msg.clone()),
                        refusal,
                        moved_from: None,
                        facts: Vec::new(),
                    });
                }
                errors.push(msg);
            }
        }
    }

    // Summary (text mode only; JSON mode consumers check `status` fields).
    let total = succeeded + errors.len();
    if !errors.is_empty() {
        if output == OutputMode::Text {
            eprintln!(
                "rwv {verb}: {succeeded}/{total} repo(s) succeeded, {} failed:",
                errors.len()
            );
            for msg in &errors {
                eprintln!("  - {msg}");
            }
        }
        // Under `--json` the caller prints the report before refusing, so a
        // partial failure still leaves parseable output.
        return Err(Box::new(MembersAborted {
            report: Some(report),
            error: crate::refusal::refusal(
                RefusalKind::PartialRunAborted,
                format!(
                    "{verb} completed with {} clone failure(s) out of {total} repo(s)",
                    errors.len()
                ),
            ),
        }));
    }

    if output == OutputMode::Text {
        outln!("rwv {verb}: done ({succeeded} repo(s) ready)");
    }

    Ok(MembersRun {
        report,
        moved_or_created,
        pins,
    })
}

impl MembersReport {
    fn outcomes_merge(&mut self, aligned: &AlignRun) {
        for (path, moved_from, facts) in &aligned.aligned {
            if let Some(outcome) = self.outcomes.iter_mut().find(|o| &o.path == path) {
                outcome.moved_from.clone_from(moved_from);
                outcome.facts.clone_from(facts);
            }
        }
        for (path, message, refusal) in &aligned.failed {
            if let Some(outcome) = self.outcomes.iter_mut().find(|o| &o.path == path) {
                outcome.status = MemberOutcomeStatus::Failed;
                outcome.message = Some(message.clone());
                outcome.refusal.clone_from(refusal);
            }
        }
    }
}

#[derive(Default)]
struct AlignRun {
    changed: bool,
    aligned: Vec<(String, Option<String>, Vec<crate::diagnosis::FactOutput>)>,
    failed: Vec<(String, String, Option<String>)>,
    failures: Vec<String>,
    judged: Vec<JudgedOutput>,
    placed: Vec<(RepoPath, PathBuf, crate::share::FilePlacement)>,
}

fn refuse_detached_project(aligner: &Aligner, verb: &'static str) -> anyhow::Result<()> {
    if aligner.standing != ProjectStanding::Detached {
        return Ok(());
    }
    Err(aligner
        .project_detached()?
        .by_verb(verb)
        .judge(crate::vcs::project_vcs().as_ref())
        .refused())
}

/// Put each selected member's checkout where the checked-out branch says,
/// moving only those the engine may move, and report the rest.
fn align_checkouts(
    ctx: &WorkspaceContext,
    aligner: &Aligner,
    project_dir: &Path,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
    verb: &str,
) -> anyhow::Result<AlignRun> {
    let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let lock_path = LockFile::path_in(project_dir);
    let lock = if lock_path.exists() {
        Some(LockFile::from_state(&lock_path)?)
    } else {
        None
    };
    let items: Vec<(RepoPath, RepoEntry, Box<dyn Vcs>, PathBuf)> = manifest
        .iter_entries()
        .filter(|(rp, entry)| selection.covers(rp, entry.role))
        .map(|(rp, entry)| {
            (
                rp.clone(),
                entry.clone(),
                vcs_for(entry.vcs_type),
                ctx.active_path().join(rp.as_path()),
            )
        })
        .filter(|(_, _, _, dir)| {
            dir.exists()
                && !crate::workweave::is_symlinked_member(dir)
                && !crate::workweave::is_dangling_link(dir)
        })
        .collect();
    let results = run_in_parallel(&items, jobs, |_idx, (rp, entry, vcs, dir)| {
        let member = Member {
            path: rp,
            entry,
            dir,
            vcs: vcs.as_ref(),
        };
        let before = vcs.head_revision(dir).ok();
        let applied = aligner
            .plan(&member, lock.as_ref())
            .and_then(|plan| aligner.apply(&member, &plan, rp));
        let files = match &applied {
            Ok(Applied::Aligned { .. }) => {
                crate::settle::settle_moved(vcs.as_ref(), dir, entry.role, before.as_ref())
                    .map(|settled| settled.files)
            }
            _ => Ok(None),
        };
        let moved = before != vcs.head_revision(dir).ok();
        (applied, files, moved)
    });
    let mut run = AlignRun::default();
    let mut left = crate::diagnosis::Report::default();
    let say = |line: String| {
        if output == OutputMode::Text {
            outln!("{line}");
        } else {
            eprintln!("{line}");
        }
    };
    for ((rp, _, _, dir), (result, files, moved)) in items.iter().zip(results) {
        run.changed |= moved;
        let (result, files) = match files {
            Ok(files) => (result, files),
            Err(e) => (Err(e.context(rp.to_string())), None),
        };
        if let Some(files) = files {
            run.placed.push((rp.clone(), dir.clone(), files));
            if output == OutputMode::Text {
                outln!("rwv {verb}: {rp}: files {files}");
            }
        }
        match result {
            Ok(Applied::Aligned {
                moved_from,
                facts,
                notes,
            }) => {
                if let Some(from) = &moved_from {
                    say(format!("rwv {verb}: {rp}: moved from {from}"));
                }
                for fact in &facts {
                    say(format!(
                        "rwv {verb}: {rp}: {}",
                        crate::diagnosis::fact_text(fact)
                    ));
                }
                for note in notes {
                    left.add(note);
                }
                run.aligned.push((
                    rp.to_string(),
                    moved_from,
                    facts
                        .iter()
                        .map(|fact| crate::diagnosis::FactOutput {
                            fact: fact.kind,
                            about: fact.about.clone(),
                        })
                        .collect(),
                ));
            }
            Ok(Applied::Left { judged }) => {
                let refused = judged
                    .iter()
                    .any(|line| line.state().axis() == crate::diagnosis::Axis::PushBranch);
                if refused {
                    let mut report = crate::diagnosis::Report::default();
                    for line in judged {
                        report.add(line);
                    }
                    let error = report
                        .into_result()
                        .expect_err("a report holding a line refuses");
                    let message = format!("{rp}: {error:#}");
                    run.failed.push((
                        rp.to_string(),
                        message.clone(),
                        crate::refusal::kind_of(&error).map(RefusalKind::token),
                    ));
                    run.failures.push(message);
                } else {
                    for line in judged {
                        left.add(line);
                    }
                }
            }
            Err(error) => {
                let message = format!("{rp}: {error:#}");
                run.failed.push((
                    rp.to_string(),
                    message.clone(),
                    crate::refusal::kind_of(&error).map(RefusalKind::token),
                ));
                run.failures.push(message);
            }
        }
    }
    if !left.is_empty() {
        if output == OutputMode::Text {
            outln!("{}", left.render(verb).trim_end_matches('\n'));
        }
        run.judged = left.output();
    }
    Ok(run)
}

/// What the clones under a run's store root are to the workspace whose lock
/// it reads.
#[derive(Clone, Copy)]
pub(crate) enum CloneDuty<'a> {
    /// They are its checkouts: a missing one is cloned, a present one is left
    /// for alignment.
    Checkouts,
    /// They are the primary's canonical clones, which a workweave's worktrees
    /// share. The workweave's pins are fetched into them and nothing they have
    /// checked out moves; the primary's lock, not the workweave's, says where a
    /// clone not on disk is born.
    SharedStore { primary_lock: Option<&'a LockFile> },
}

/// Per-repo worker of [`bring_members`]: one iteration of its loop body.
///
/// Returns [`MemberStep`]; the caller aggregates failures.
///
/// When `output` is a JSON mode, progress lines are emitted to stderr (via
/// `reporter.err`) rather than stdout, so that stdout carries only
/// machine-readable JSON output.
#[allow(clippy::too_many_arguments)]
fn bring_one(
    verb: &'static str,
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    workspace_root: &Path,
    existing_lock: Option<&LockFile>,
    duty: CloneDuty<'_>,
    selection: &MemberSelection,
    reporter: &Reporter<'_>,
    output: OutputMode,
) -> MemberStep {
    // Helper: route progress to stdout (text mode) or stderr (JSON mode).
    let emit = |msg: &str| {
        if output.is_json() {
            reporter.err(msg);
        } else {
            reporter.out(msg);
        }
    };

    if selection.leaves_out(entry.role) {
        emit(&format!(
            "rwv {verb}: skipping {} (role: reference)",
            repo_path.as_str()
        ));
        return MemberStep::Skipped;
    }

    let dest: PathBuf = workspace_root.join(repo_path.as_path());

    let lock_entry = existing_lock.and_then(|l| l.get_entry(repo_path).cloned());

    if let CloneDuty::SharedStore { primary_lock } = duty {
        if !dest.exists() {
            let birth = primary_lock
                .and_then(|l| l.get_entry(repo_path))
                .or(lock_entry.as_ref());
            if let Err(error) = clone_member(verb, vcs, repo_path, entry, &dest, birth, &emit) {
                return MemberStep::Failed(error);
            }
        }
        return match lock_entry {
            Some(pin) => make_pin_local(verb, vcs, repo_path, entry, &dest, &pin.version, &emit),
            None => MemberStep::Ok {
                pin: PinOutcome::None,
            },
        };
    }

    if dest.exists() {
        let pin = match lock_entry {
            None => PinOutcome::None,
            Some(lock_entry) => match vcs.resolve_revision(&dest, lock_entry.version.as_str()) {
                Ok(_) => PinOutcome::Reached,
                Err(_) => PinOutcome::Unreachable,
            },
        };
        return MemberStep::Ok { pin };
    }

    match clone_member(
        verb,
        vcs,
        repo_path,
        entry,
        &dest,
        lock_entry.as_ref(),
        &emit,
    ) {
        Ok(pin) => MemberStep::Ok { pin },
        Err(error) => MemberStep::Failed(error),
    }
}

/// Clone the member at `repo_path` into `dest`: born attached to the local
/// counterpart of the branch the manifest declares, at `birth`'s pin where the
/// clone holds it, else at that branch's tip on the remote.
fn clone_member(
    verb: &'static str,
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    dest: &Path,
    birth: Option<&LockEntry>,
    emit: &dyn Fn(&str),
) -> anyhow::Result<PinOutcome> {
    let clone_url = registry::resolve_to_clone_info(&entry.url)
        .with_context(|| repo_path.to_string())?
        .url;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "{}: failed to create directory {}",
                repo_path.as_str(),
                parent.display()
            )
        })?;
    }

    emit(&format!(
        "rwv {verb}: cloning {} from {} (role: {})",
        repo_path.as_str(),
        clone_url,
        entry.role
    ));

    // A lock-pinned clone is born ATTACHED AT THE PIN — one operation, not a
    // clone followed by an align. Cloning to the remote tip and then moving
    // to the pin would leave the member detached, which is not a state a
    // clone is entitled to put an operator's checkout in.
    let counterpart = entry.branch.local_counterpart();
    let born = vcs
        .clone_attached_at(
            &clone_url,
            dest,
            &counterpart,
            &entry.branch.on_remote(),
            birth.map(|pin| &pin.version),
        )
        .with_context(|| {
            format!(
                "{}: failed to materialize {} on branch '{}'",
                repo_path.as_str(),
                clone_url,
                entry.branch,
            )
        })?;
    match (born.pin, birth) {
        (PinOutcome::Reached, Some(pin)) => emit(&format!(
            "rwv {verb}: cloned {} onto {counterpart} at {}",
            repo_path.as_str(),
            pin.version,
        )),
        (PinOutcome::Unreachable, Some(pin)) => emit(&format!(
            "rwv {verb}: cloned {} onto {counterpart} at the branch tip: the clone holds no \
             {}",
            repo_path.as_str(),
            pin.version,
        )),
        _ => emit(&format!(
            "rwv {verb}: cloned {} onto {counterpart} at the branch tip: the lock has no entry \
             for it",
            repo_path.as_str(),
        )),
    }
    Ok(born.pin)
}

/// Make `pin` resolvable in the canonical clone at `store`, fetching from its
/// remote only when it is not already there. Nothing checked out moves.
fn make_pin_local(
    verb: &'static str,
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    store: &Path,
    pin: &RawRevisionId,
    emit: &dyn Fn(&str),
) -> MemberStep {
    let reached = MemberStep::Ok {
        pin: PinOutcome::Reached,
    };
    if vcs.resolve_revision(store, pin.as_str()).is_ok() {
        return reached;
    }
    emit(&format!(
        "rwv {verb}: fetching {pin} into the canonical clone of {}",
        repo_path.as_str()
    ));
    let url_remote = match registry::resolve_to_clone_info(&entry.url)
        .and_then(|info| crate::url_remote::ensure(vcs, repo_path, verb, store, &info.url))
    {
        Ok(remote) => remote,
        Err(e) => {
            return MemberStep::Failed(e.context(format!(
                "{}: no url remote to fetch from",
                repo_path.as_str()
            )));
        }
    };
    if let Err(e) = vcs.fetch_remote(store, &url_remote) {
        return MemberStep::Failed(anyhow::Error::from(e).context(format!(
            "{}: failed to fetch {url_remote} into the canonical clone",
            repo_path.as_str(),
        )));
    }
    match vcs.resolve_revision(store, pin.as_str()) {
        Ok(_) => reached,
        Err(e) => MemberStep::Failed(anyhow::Error::from(e).context(format!(
            "{}: {pin} is not on {url_remote} either",
            repo_path.as_str(),
        ))),
    }
}
