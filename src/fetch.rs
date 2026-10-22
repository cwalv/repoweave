//! `rwv fetch` — clone a project and its repos into the workspace.
//!
//! The source must be a URL (full URL or `owner/repo` shorthand resolved via
//! the registry). A plain path is refused: a local remote is spelled
//! `file://`, and a project already in the weave is fetched without a source.

use crate::activate::ActivateOptions;
use crate::cli::consent::DetachConsent;
use crate::lock;
use crate::manifest::{LockFile, Manifest, RepoEntry, RepoPath, Role};
use crate::parallel::{run_in_parallel, OutputMode, Reporter};
use crate::placement::PlacementOutput;
use crate::refusal::{refusal, RefusalKind};
use crate::registry;
use crate::selector::RepoFilter;
use crate::skipped::{SkipReason, SkippedEffect};
use crate::vcs::{
    project_vcs, vcs_for, HeadAttachment, ReplayExclusionState, ResolvedRevisionId, Vcs, VcsError,
};
use crate::workspace::{
    observe_root, project_dir, projects_dir, Checkout, Resolution, RootObservation,
    WorkspaceContext,
};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Schema URL embedded in `rwv fetch --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/fetch.json`.
pub const FETCH_SCHEMA_URL: &str = crate::schema_url::schema_url!("fetch");

/// Schema URL embedded in each `rwv fetch --json -j N` NDJSON record line.
/// Pins to `docs/reference/schemas/fetch-record.json`, which describes the
/// flat per-repo wire shape — distinct from the serial envelope (`fetch.json`).
pub const FETCH_RECORD_SCHEMA_URL: &str = crate::schema_url::schema_url!("fetch-record");

/// Per-repo outcome record for `rwv fetch --json`.
///
/// `status` is one of `"ok"`, `"skipped"`, or `"failed"`. `message` carries
/// a human-readable description of the outcome (always present for `"failed"`;
/// present for `"skipped"` to say why; `null` for `"ok"`).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchOutcomeOutput {
    pub path: String,
    pub absolute_path: crate::path_spelling::WirePath,
    pub status: FetchOutcomeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl FetchOutcomeOutput {
    pub fn is_failure(&self) -> bool {
        self.status == FetchOutcomeStatus::Failed
    }
}

/// Status discriminant for [`FetchOutcomeOutput`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FetchOutcomeStatus {
    Ok,
    Skipped,
    Failed,
}

/// Top-level envelope for `rwv fetch --json` (serial / `-j 1` mode).
///
/// Shape: `{ "$schema": "<url>", "outcomes": [<FetchOutcomeOutput>, ...] }`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub outcomes: Vec<FetchOutcomeOutput>,
    /// How the files of each checkout the fetch moved or created were placed:
    /// a canonical clone it realigned, and from a workweave each worktree it
    /// created there. Empty, not absent, when it placed none.
    pub placements: Vec<PlacementOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// The materialization side effects this fetch skipped, each naming the
    /// command that performs it. Empty, not absent, when it skipped none.
    #[serde(default)]
    pub skipped: Vec<SkippedEffect>,
}

impl FetchJsonOutput {
    fn print(&self) -> anyhow::Result<()> {
        let out = serde_json::to_string_pretty(self).context("failed to serialize fetch output")?;
        outln!("{out}");
        Ok(())
    }
}

/// One NDJSON record emitted by `rwv fetch --json -j N` with `N > 1`.
///
/// Each per-repo outcome becomes its own self-describing line. Every record
/// carries its own `$schema` URL (pointing at `fetch-record.json`) so
/// consumers can identify a line without out-of-band context. Serialized with
/// `#[serde(flatten)]` so the wire shape is a single flat object:
/// `{"$schema": "...", "path": "...", ...}`.
///
/// `$schema` names `docs/reference/schemas/fetch-record.json`, not the
/// envelope artifact (`fetch.json`), because the two documents have different
/// shapes: the envelope wraps an `outcomes` array; each streamed line is one
/// flat per-repo object.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FetchOutcomeNdjsonRecord<'a> {
    #[serde(rename = "$schema")]
    pub schema: &'a str,
    #[serde(flatten)]
    pub outcome: &'a FetchOutcomeOutput,
}

/// Controls how `rwv fetch` resolves repo versions.
///
/// - `Default`: read `rwv.lock` and align clones to it. The lock is the
///   source of truth for which revision each repo should be at. When the
///   lock is absent, fetch bootstraps it from branch HEAD (one-time event).
///   When a manifest entry is missing from the lock, it is added at branch
///   HEAD (additive only — never moves existing SHAs).
/// - `Frozen`: like `Default`, but errors if the lock file is missing or
///   does not cover all manifest repos (CI mode). Never mutates the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMode {
    /// Read rwv.lock, align clones, bootstrap or add missing entries
    /// additively.
    Default,
    /// Like Default, but error when the lock is absent or incomplete
    /// rather than writing it. Clones are aligned the same way.
    Frozen,
}

/// Resolve `source` to a clone URL.
///
/// Accepts full URLs (returned as-is) or `owner/repo` / `registry/owner/repo`
/// shorthand (resolved via the built-in registries to an HTTPS clone URL).
fn resolve_source(source: &str) -> anyhow::Result<registry::CloneUrl> {
    let parsed: crate::manifest::RepoUrl = source.parse()?;
    if parsed.is_plain_path() {
        crate::refuse!(
            RefusalKind::UnresolvableRepoSource,
            "rwv fetch: '{source}' is a plain path, which names no remote to clone the \
             project from. To clone from a local remote, spell it as a URL: \
             `rwv fetch {file_url}`. To fetch the members of a project already in this \
             weave at projects/<name>/, drop SOURCE: `rwv fetch --project <name>`.",
            file_url = crate::path_spelling::file_url_suggestion(source),
        );
    }
    Ok(registry::resolve_to_clone_info(&parsed)?.url)
}

/// Which members of a manifest a fetch acts on: the ones `--role` and `--repo`
/// name, less the `reference`-role ones under `--no-reference`. Decided once,
/// where the flags are parsed; every step of the fetch asks it.
#[derive(Debug, Clone)]
pub struct MemberSelection {
    filter: RepoFilter,
    skip_reference: bool,
}

impl MemberSelection {
    pub fn new(filter: RepoFilter, no_reference: bool) -> Self {
        Self {
            filter,
            skip_reference: no_reference,
        }
    }

    /// Whether `--role` / `--repo` name this member, or name none at all.
    fn names(&self, repo_path: &RepoPath, role: Role) -> bool {
        self.filter.matches(repo_path, role)
    }

    /// Whether `--no-reference` opts this member out of the fetch.
    fn leaves_out(&self, role: Role) -> bool {
        self.skip_reference && role == Role::Reference
    }

    /// Whether the fetch acts on this member.
    fn covers(&self, repo_path: &RepoPath, role: Role) -> bool {
        self.names(repo_path, role) && !self.leaves_out(role)
    }

    /// Whether no `--role` or `--repo` narrows the fetch.
    fn names_every_member(&self) -> bool {
        self.filter.is_empty()
    }
}

/// Validate that a lock file covers all repos in the manifest.
///
/// Returns a list of repo paths present in the manifest but missing from the lock.
/// A member the selection leaves out is not checked — the user has opted out
/// of fetching it, so a missing lock entry for it shouldn't fail `--frozen`.
fn find_incomplete_repos(
    manifest: &Manifest,
    lock: &LockFile,
    selection: &MemberSelection,
) -> Vec<RepoPath> {
    manifest
        .iter_entries()
        .filter(|(_, entry)| !selection.leaves_out(entry.role))
        .map(|(rp, _)| rp)
        .filter(|rp| !lock.contains_repo(rp))
        .cloned()
        .collect()
}

/// Per-repo outcome of the fetch loop body. Communicates back to the
/// caller (1) whether the repo succeeded, (2) whether it should be added
/// to the lock under additive bootstrap rules, and (3) what to print in
/// the error report on failure.
enum FetchOutcome {
    /// Repo handled successfully. `add_to_lock` is set iff the repo was
    /// new to an existing lock and needs an additive entry post-join.
    Ok { add_to_lock: Option<RepoPath> },
    /// Repo intentionally skipped (e.g. `--no-reference` on a reference
    /// repo). Not counted as failure; not counted as success.
    Skipped,
    /// Repo failed; `msg` carries the aggregated-error-report message.
    Failed { msg: String },
}

/// Run the fetch command: clone a project source, then align repos to the
/// lock file (bootstrapping it if necessary in Default mode).
///
/// `workspace_root` is the directory where repos and `projects/` live (CWD).
///
/// Lock mutation:
/// - `Default`: bootstrap the lock from branch HEAD when absent; otherwise
///   read existing entries and additively add missing ones at branch HEAD.
///   Never advances entries that already exist in the lock.
/// - `Frozen`: never writes the lock; errors if missing or incomplete (CI mode).
///
/// `jobs` is the resolved worker count (post-[`crate::parallel::resolve_jobs`]).
/// `jobs == 1` runs serially with no prefix; `jobs > 1` runs the per-repo
/// clone/checkout loop on a bounded worker pool, prefixing each line with
/// the repo path. Project-level steps (cloning the project repo,
/// validating the lock, writing the lock at the end, auto-activate) all
/// happen serially.
///
/// When `output` is a JSON mode:
/// - `jobs == 1`: emits a single `{ "$schema": ..., "outcomes":
///   [...] }` envelope to stdout after all repos complete.
/// - `jobs > 1`: streams one self-describing NDJSON line per repo to stdout
///   as each worker finishes, then emits no envelope wrapper.
#[allow(clippy::too_many_arguments)]
pub fn run_fetch(
    source: &str,
    workspace_root: &Path,
    mode: FetchMode,
    selection: &MemberSelection,
    detach_checkouts: Option<DetachConsent>,
    jobs: usize,
    output: OutputMode,
    activation: ActivateOptions,
) -> anyhow::Result<()> {
    let project_vcs = project_vcs();

    // Resolve source to a clone URL (supports full URLs and owner/repo shorthand).
    let url = resolve_source(source)?;
    let name = crate::manifest::ProjectName::new(registry::repo_name_from_source(url.as_str()))?;
    let projects_dir = projects_dir(workspace_root);
    std::fs::create_dir_all(&projects_dir).context("failed to create projects/ directory")?;
    if let Some(crate::workspace::RootObservation::Primary { pointer, .. }) =
        crate::workspace::observe_root(workspace_root)
    {
        pointer.selection()?;
    }
    let project_dir = crate::workspace::project_dir(workspace_root, &name);
    if project_dir.exists() {
        crate::refuse!(
            RefusalKind::ProjectDirOccupied,
            "cannot fetch project '{name}': {}\n\
             Hint: if that is this project, `rwv fetch --project {name}` fetches its \
             members in place. To fetch this one beside it, run `rwv fetch {source}` in an \
             empty directory, which becomes a weave of its own.",
            crate::workspace::describe_existing(&project_dir)
        );
    } else {
        // In JSON mode, project-level progress goes to stderr so stdout stays
        // JSON-only. In text mode it goes to stdout as before.
        if output.is_json() {
            eprintln!("rwv fetch: cloning project '{}'", name);
        } else {
            outln!("rwv fetch: cloning project '{}'", name);
        }
        project_vcs
            .clone_repo(&url, &project_dir)
            .with_context(|| format!("failed to clone project source '{}'", url))?;

        // Unlike `init`/`init_adopt`, fetch never calls `set_replay_exclusion`
        // — it clones a project that already exists, and whether `rwv.lock`
        // is declared derived is that project's own prior choice. Complete
        // the config only when the clone already carries the declaration;
        // otherwise there is no `rwv-ours` assignment for the driver to serve.
        let declares_rwv_ours = matches!(
            project_vcs
                .replay_exclusion_state(&project_dir, Path::new(LockFile::FILE_NAME))
                .with_context(|| format!(
                    "failed to read .gitattributes in {}",
                    project_dir.display()
                ))?,
            ReplayExclusionState::Current | ReplayExclusionState::LegacyAlongsideCurrent
        );
        if declares_rwv_ours {
            crate::git::plant_rwv_merge_driver_config(&project_dir)
                .context("failed to plant merge.rwv-ours config")?;
        }
    }

    let envelope = fetch_project_repos(
        &name,
        &project_dir,
        workspace_root,
        mode,
        selection,
        detach_checkouts,
        jobs,
        output,
        Some(activation),
        // Source-mode fetch runs before any WorkspaceContext is resolved; no
        // resolution block is available at this point.
        None,
    )?
    .envelope;
    if output == OutputMode::JsonEnvelope {
        envelope.print()?;
    }
    Ok(())
}

/// In-place mode: re-materialize missing manifest members for the active
/// project in the current workspace, aligning each clone to `rwv.lock` (or
/// branch HEAD if the lock has no entry for the missing repo).
///
/// This is the settled repair verb for a dangling reference: no SOURCE
/// argument, resolves the workspace from CWD, iterates the active project's
/// manifest, and clones any repo whose canonical clone directory is missing.
///
/// A clone that is already present is realigned rather than skipped: when the
/// lock covers it, `fetch_one` resolves the locked revision in that clone's
/// own object store and `realign_present_clone` advances the checkout to it
/// — fast-forwarding the tracking branch's local counterpart, or refusing
/// unless `detach_checkouts` carries a consent. When the lock has no entry for
/// the repo, or there is no lock, the clone is left alone and the lock records
/// its current HEAD.
///
/// Clone-topology (I1): the canonical clone always lives at primary's
/// `<weave>/<repo_path>`, even when this verb is invoked from inside a
/// workweave. In-place fetch therefore materializes into
/// [`WorkspaceContext::primary_path`] first; from a workweave it then creates
/// the worktree for each member the workweave's `rwv.lock` pins and the
/// workweave lacks, as `rwv doctor --fix` would once the revision is local.
///
/// When the primary has no project selected and the invocation named none,
/// the weave's only project is fetched and then activated; with zero or
/// several projects there is nothing to select and the run refuses, naming
/// `--project`.
///
/// Flag semantics: `--frozen`, `--no-reference`, `--role`/`--repo`, `-j`,
/// `--json` all carry the same meaning as the SOURCE-mode path. Filtered
/// runs are additive and skip the lock-write step (same rule as bootstrap).
#[allow(clippy::too_many_arguments)]
pub fn run_fetch_in_place(
    ctx: &WorkspaceContext,
    mode: FetchMode,
    selection: &MemberSelection,
    detach_checkouts: Option<DetachConsent>,
    jobs: usize,
    output: OutputMode,
    activation: ActivateOptions,
) -> anyhow::Result<()> {
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

    // Per-workspace state (rwv.toml, rwv.lock) lives under active_path — the
    // workweave when in one, primary otherwise. Mirrors add_remove.rs's
    // find_project_dir.
    let project_dir = project_dir(ctx.active_path(), &name);

    // Clone destination (canonical store) is always primary's slot — clone-
    // topology I1. Passing primary_path() as workspace_root routes fetch_one
    // to write clones at `<primary>/<repo_path>/`, even when CWD is inside a
    // workweave. See docs/explanation/joints/clone-topology.md.
    let workspace_root = ctx.primary_path();

    let FetchRun {
        mut envelope,
        moved_or_created,
    } = fetch_project_repos(
        &name,
        &project_dir,
        workspace_root,
        mode,
        selection,
        detach_checkouts,
        jobs,
        output,
        None,
        ctx.resolution(),
    )?;

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
            &mut envelope.placements,
        ),
        Checkout::Primary { .. } => Ok(false),
    };
    // In a workweave the clones fetch moves are the primary's; only a worktree
    // it created is a checkout of this workweave's.
    let checkout_changed = match &ctx.checkout {
        Checkout::Workweave { .. } => matches!(created, Ok(true)),
        Checkout::Primary { .. } => moved_or_created,
    };
    if let Some(project) = &sole {
        let msg = format!(
            "rwv fetch: activating project '{project}', the only project in this weave \
             (none was active)"
        );
        if output == OutputMode::Text {
            outln!("{msg}");
        } else {
            eprintln!("{msg}");
        }
        envelope
            .skipped
            .extend(crate::activate::activate_with_options(
                project, invocation, activation,
            )?);
    } else if checkout_changed {
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
        envelope.skipped.push(skip);
    }
    if output == OutputMode::JsonEnvelope {
        envelope.print()?;
    }
    created.map(|_| ())
}

/// The project an in-place fetch selects for itself: the weave's only one,
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

/// Create the worktree for each member the workweave's `rwv.lock` pins and the
/// workweave lacks — nothing at the slot, or a symlink — once the fetch above
/// has put the pinned revisions in the canonical clones, returning whether it
/// created any. How each one's files were placed is streamed under NDJSON, and
/// added to `placements` for the envelope.
#[allow(clippy::too_many_arguments)]
fn materialize_workweave_members(
    ctx: &WorkspaceContext,
    project: &crate::manifest::ProjectName,
    workweave: &crate::manifest::WorkweaveName,
    project_dir: &Path,
    selection: &MemberSelection,
    output: OutputMode,
    placements: &mut Vec<PlacementOutput>,
) -> anyhow::Result<bool> {
    let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let lock_path = LockFile::path_in(project_dir);
    if !lock_path.exists() {
        return Ok(false);
    }
    let lock = LockFile::from_state(&lock_path)?;
    let mut failures: Vec<String> = Vec::new();
    let mut created = false;
    for (repo_path, entry) in manifest.iter_entries() {
        if !selection.covers(repo_path, entry.role) {
            continue;
        }
        let Some(pin) = lock.get_entry(repo_path) else {
            continue;
        };
        let slot = ctx.active_path().join(repo_path.as_path());
        if !crate::workweave::is_symlinked_member(&slot) && slot.exists() {
            continue;
        }
        match crate::workweave::materialize_member(
            vcs_for(entry.vcs_type).as_ref(),
            ctx.primary_path(),
            ctx.active_path(),
            project,
            workweave,
            repo_path,
            entry.role,
            entry.submodules,
            pin.version.as_str(),
        ) {
            Ok(born) => {
                created = true;
                let line = format!(
                    "rwv fetch: created the worktree for {repo_path} in this workweave at {}{}",
                    born.at.id().short(),
                    crate::share::files_note(born.files),
                );
                if output == OutputMode::Text {
                    outln!("{line}");
                } else {
                    eprintln!("{line}");
                }
                if let Some(files) = born.files {
                    let placement = PlacementOutput::new(repo_path, &slot, files);
                    match output {
                        OutputMode::Text => {}
                        OutputMode::JsonEnvelope => placements.push(placement),
                        OutputMode::Ndjson => placement.stream(&Mutex::new(())),
                    }
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
        "fetch aligned the canonical clones but could not create {} worktree(s) in this \
         workweave:\n  {}",
        failures.len(),
        failures.join("\n  ")
    )
}

/// Shared per-project repo-materialization loop for both SOURCE-mode
/// (bootstrap: `rwv fetch <source>`) and in-place mode (`rwv fetch` with no
/// SOURCE). Both entries route through this helper so the clone/checkout,
/// lock-alignment, and lock-write behavior stays identical across both
/// invocations.
///
/// `auto_activate` carries the options of the first-fetch auto-activate step,
/// and is `None` where there is none to take: only SOURCE-mode has one — the
/// in-place path selects its project itself, in [`run_fetch_in_place`].
///
/// `resolution` is the resolved workspace coordinate block for the
/// `--json` envelope. `None` when no `WorkspaceContext` is available (the
/// SOURCE-mode bootstrap path runs before a context is resolved).
///
/// Returns the `--json` envelope for the caller to print once it has placed
/// anything else, so it is populated only in envelope mode, with whether any
/// checkout moved or was created. A run with failures prints the envelope
/// here, before refusing.
#[allow(clippy::too_many_arguments)]
fn fetch_project_repos(
    name: &crate::manifest::ProjectName,
    project_dir: &Path,
    workspace_root: &Path,
    mode: FetchMode,
    selection: &MemberSelection,
    detach_checkouts: Option<DetachConsent>,
    jobs: usize,
    output: OutputMode,
    auto_activate: Option<ActivateOptions>,
    resolution: Option<Resolution>,
) -> anyhow::Result<FetchRun> {
    // Read the manifest
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to read manifest from {}", manifest_path.display()))?;

    // Load the lock file. In Frozen mode the lock must exist and cover all
    // manifest repos. In Default mode the lock may be absent (bootstrap), and
    // missing repos are added additively after fetch.
    let lock_path = LockFile::path_in(project_dir);
    let existing_lock: Option<LockFile> = if lock_path.exists() {
        Some(
            LockFile::from_state(&lock_path)
                .with_context(|| format!("failed to read lock file at {lock_path}"))?,
        )
    } else {
        None
    };

    match mode {
        FetchMode::Frozen => {
            let lock = existing_lock.as_ref().ok_or_else(|| {
                refusal(
                    RefusalKind::MissingLock,
                    format!("rwv fetch --frozen: lock file does not exist at {lock_path}"),
                )
            })?;
            let missing = find_incomplete_repos(&manifest, lock, selection);
            if !missing.is_empty() {
                let names: Vec<&str> = missing.iter().map(|rp| rp.as_str()).collect();
                crate::refuse!(
                    RefusalKind::IncompleteLock,
                    "rwv fetch --frozen: lock file is incomplete; repos not covered by lock: {}",
                    names.join(", ")
                );
            }
        }
        FetchMode::Default => {
            // Lock-not-present is a normal bootstrap. Missing entries are
            // additive and dealt with below.
        }
    }

    // Warn about orphan lock entries (in lock, not in manifest). Doesn't fail
    // and doesn't touch the clones.
    if let Some(ref lock) = existing_lock {
        for repo_path in lock.iter_repo_paths() {
            if !manifest.contains_repo(repo_path) {
                eprintln!(
                    "rwv fetch: warning: orphan in lock: {} (lock entry has no manifest entry)",
                    repo_path.as_str()
                );
            }
        }
        for (repo_path, version) in lock.entries_pinned_by_name() {
            eprintln!(
                "rwv fetch: warning: {}",
                lock::pinned_by_name_warning(repo_path, version)
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

    let outcomes: Vec<(FetchOutcome, Option<crate::share::FilePlacement>, bool)> =
        run_in_parallel(&work_items, jobs, |_idx, item| {
            let (repo_path, entry, vcs) = item;
            let reporter = if parallel && output == OutputMode::Text {
                // Text parallel mode: use prefixed reporter.
                Reporter::parallel(repo_path.as_str().to_string(), &write_lock)
            } else if parallel {
                // JSON parallel mode: suppress text output entirely; JSON is
                // emitted below. Use a no-op serial reporter so fetch_one's
                // reporter.out() calls do nothing visible.
                Reporter::serial()
            } else {
                Reporter::serial()
            };
            let member = workspace_root.join(repo_path.as_path());
            let before = vcs.head_revision(&member).ok();
            let outcome = fetch_one(
                vcs.as_ref(),
                repo_path,
                entry,
                workspace_root,
                existing_lock.as_ref(),
                selection,
                detach_checkouts,
                &reporter,
                output,
            );
            let (outcome, files) = match outcome {
                FetchOutcome::Ok { .. } => {
                    match crate::settle::settle_moved(
                        vcs.as_ref(),
                        &member,
                        entry.role,
                        before.as_ref(),
                    ) {
                        Ok(settled) => (outcome, settled.files),
                        Err(e) => (
                            FetchOutcome::Failed {
                                msg: format!("{}: {e:#}", repo_path.as_str()),
                            },
                            None,
                        ),
                    }
                }
                other => (other, None),
            };
            let moved_or_created = matches!(outcome, FetchOutcome::Ok { .. })
                && before != vcs.head_revision(&member).ok();

            // NDJSON mode: emit one line per repo as soon as it finishes.
            if output.is_ndjson() {
                if let Some(files) = files {
                    PlacementOutput::new(repo_path, &member, files).stream(&write_lock);
                }
                let abs_path = crate::path_spelling::WirePath::of(&member);
                let record = match &outcome {
                    FetchOutcome::Ok { .. } => FetchOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: FetchOutcomeStatus::Ok,
                        message: None,
                    },
                    FetchOutcome::Skipped => FetchOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: FetchOutcomeStatus::Skipped,
                        message: Some(format!("skipped {}", repo_path.as_str())),
                    },
                    FetchOutcome::Failed { msg } => FetchOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: FetchOutcomeStatus::Failed,
                        message: Some(msg.clone()),
                    },
                };
                let ndjson_line = FetchOutcomeNdjsonRecord {
                    schema: FETCH_RECORD_SCHEMA_URL,
                    outcome: &record,
                };
                crate::stdout::stream_record(&write_lock, &ndjson_line);
            }

            (outcome, files, moved_or_created)
        });

    // Aggregate outcomes serially in input order — preserves the existing
    // error-report shape and gives the lock-write step a deterministic
    // `added_to_lock` list.
    let mut succeeded = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut added_to_lock: Vec<RepoPath> = Vec::new();

    // For envelope mode (json && !ndjson), collect records in work_items order.
    let mut envelope = FetchJsonOutput {
        schema: FETCH_SCHEMA_URL.to_owned(),
        outcomes: Vec::new(),
        placements: Vec::new(),
        resolution,
        skipped: Vec::new(),
    };
    let envelope_records = &mut envelope.outcomes;

    let moved_or_created = outcomes.iter().any(|(_, _, moved)| *moved);
    for ((outcome, files, _), (repo_path, _entry, _vcs)) in outcomes.iter().zip(work_items.iter()) {
        let member = workspace_root.join(repo_path.as_path());
        let abs_path = crate::path_spelling::WirePath::of(&member);
        match outcome {
            FetchOutcome::Ok { add_to_lock } => {
                succeeded += 1;
                if let Some(rp) = add_to_lock {
                    added_to_lock.push(rp.clone());
                }
                if let (OutputMode::Text, Some(files)) = (output, files) {
                    outln!("rwv fetch: {repo_path}: files {files}");
                }
                if output == OutputMode::JsonEnvelope {
                    envelope_records.push(FetchOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: FetchOutcomeStatus::Ok,
                        message: None,
                    });
                    if let Some(files) = files {
                        envelope
                            .placements
                            .push(PlacementOutput::new(repo_path, &member, *files));
                    }
                }
            }
            FetchOutcome::Skipped => {
                if output == OutputMode::JsonEnvelope {
                    envelope_records.push(FetchOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: FetchOutcomeStatus::Skipped,
                        message: Some(format!("skipped {}", repo_path.as_str())),
                    });
                }
            }
            FetchOutcome::Failed { msg } => {
                if output == OutputMode::Text {
                    eprintln!("rwv fetch: error: {msg}");
                }
                errors.push(msg.clone());
                if output == OutputMode::JsonEnvelope {
                    envelope_records.push(FetchOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: FetchOutcomeStatus::Failed,
                        message: Some(msg.clone()),
                    });
                }
            }
        }
    }

    // Summary (text mode only; JSON mode consumers check `status` fields).
    let total = succeeded + errors.len();
    if !errors.is_empty() {
        if output.is_json() {
            // In JSON mode, emit the envelope/NDJSON before exiting so
            // consumers always get parseable output even on partial failure.
            if output == OutputMode::JsonEnvelope {
                let _ = envelope.print();
            }
            // NDJSON records were already streamed; nothing extra to emit.
            crate::refuse!(
                RefusalKind::PartialRunAborted,
                "fetch completed with {} clone failure(s) out of {total} repo(s)",
                errors.len()
            )
        }
        eprintln!(
            "rwv fetch: {succeeded}/{total} repo(s) succeeded, {} failed:",
            errors.len()
        );
        for msg in &errors {
            eprintln!("  - {msg}");
        }
        crate::refuse!(
            RefusalKind::PartialRunAborted,
            "fetch completed with {} clone failure(s) out of {total} repo(s)",
            errors.len()
        )
    }

    if output == OutputMode::Text {
        outln!("rwv fetch: done ({succeeded} repo(s) ready)");
    }

    // Default mode: bootstrap or additively extend the lock.
    //
    // A non-empty `--role` / `--repo` filter narrows the fetch to a subset of
    // the manifest; non-filtered repos are not on disk (in a fresh workspace)
    // or have not been touched (in an existing workspace). Either way, the
    // lock-write paths below — both bootstrap and additive — assume they can
    // read HEAD from every manifest repo; under a filter that's false. Skip
    // the lock-write entirely under a filter: the filtered fetch is a
    // targeted-sync operation, not a lock-bumping one. Use `rwv update` or
    // unfiltered `rwv fetch` to refresh the lock.
    if mode == FetchMode::Default && selection.names_every_member() {
        let needs_bootstrap = existing_lock.is_none();
        let has_additions = !added_to_lock.is_empty();

        // generate_lock walks every entry in the manifest and runs
        // `git rev-parse HEAD` against each on-disk repo. A member the
        // selection leaves out was skipped above and its directory may not
        // exist on disk — drop it from the manifest used for lock generation
        // so we don't trip on the missing path.
        let mut lock_manifest = manifest.clone();
        lock_manifest.retain_repos(|_, entry| !selection.leaves_out(entry.role));

        if needs_bootstrap {
            // Snapshot the full set of manifest repos from disk.
            let new_lock = lock::generate_lock(&lock_manifest, workspace_root, None, true)?;
            lock::write_lock_in(&new_lock, project_dir)?;
            eprintln!("rwv fetch: wrote {lock_path}");
        } else if has_additions {
            // Existing entries keep the commits they pin. One pinned by name
            // is written as the commit the name resolves to now, which is
            // where the alignment above left its clone. Added repos are
            // snapshotted at HEAD.
            let mut merged = existing_lock
                .as_ref()
                .expect("existing_lock is Some when !needs_bootstrap")
                .clone();
            let named: Vec<RepoPath> = merged
                .entries_pinned_by_name()
                .map(|(repo_path, _)| repo_path.clone())
                .collect();
            let (resolved, _) = merged.clone().resolve_versions(workspace_root);
            for repo_path in named {
                if let Some(entry) = resolved.get_entry(&repo_path) {
                    merged.insert_entry(repo_path, entry.to_raw());
                }
            }
            // Generate a fresh lock for new entries only.
            let new_lock = lock::generate_lock(&lock_manifest, workspace_root, None, true)?;
            for repo_path in &added_to_lock {
                if let Some(entry) = new_lock.get_entry(repo_path) {
                    merged.insert_entry(repo_path.clone(), entry.to_raw());
                }
            }
            lock::write_lock_in(&merged, project_dir)?;
            eprintln!("rwv fetch: wrote {lock_path}");
        }
    }

    // Activation is not part of lock coverage: `--frozen` and a filter both
    // still activate the project a first fetch created.
    if let Some(activation) = auto_activate {
        let ctx = crate::workspace::WorkspaceContext::resolve_unbound(workspace_root)?;
        let active = match ctx.primary_identity() {
            Some(primary) => primary.pointer().selection()?.cloned(),
            None => None,
        };
        match active {
            Some(active) => {
                let skip = SkippedEffect::activation_while_active(&active, name);
                // Route to stderr in JSON mode so stdout stays JSON-only.
                if output.is_json() {
                    eprintln!("{}", skip.line());
                } else {
                    outln!("{}", skip.line());
                }
                envelope.skipped.push(skip);
            }
            None => {
                envelope
                    .skipped
                    .extend(crate::activate::activate_with_options(
                        name, &ctx, activation,
                    )?);
            }
        }
    }

    Ok(FetchRun {
        envelope,
        moved_or_created,
    })
}

/// What [`fetch_project_repos`] did, beyond what its envelope reports.
struct FetchRun {
    envelope: FetchJsonOutput,
    /// Some checkout's HEAD is not where it was before this fetch: a clone it
    /// created, or one it moved. Read whatever the output mode, unlike the
    /// envelope's `placements`, which also has no record for a fresh clone.
    moved_or_created: bool,
}

/// Realign a clone that is already on disk to `target`.
///
/// The kind of ref write this performs is decided by what HEAD is, and each
/// kind carries its own precondition:
///
/// - **Attached to the tracking declaration's local counterpart** — a MOVE
///   of that branch, legal when `target` is a fast-forward. A non-fast-forward
///   (materializing an older lock, or a branch carrying commits origin has
///   not seen) refuses, naming `--detach-checkouts`.
/// - **Attached to anything else** — an operator's personal branch. Refuses
///   naming both refs rather than relocating a ref it cannot relate to the
///   layer that justifies the move. It refuses even when `target` *would* be
///   a fast-forward: attachment is operator state, and a fast-forward of a
///   personal bookmark still silently changes what it means.
/// - **Detached** — a MOVE of HEAD itself, which stays detached. Subject to
///   the mid-operation precondition inside [`Vcs::advance_detached_head`]
///   — `Detached` alone cannot tell "rwv left this at a lock SHA" apart from
///   "the operator is stopped mid-bisect".
/// - **Unborn** — refuses. Both exits are unrepresentable rather than
///   undecided: MOVE semantics on an unborn HEAD are undefined (an
///   `UnbornRef` cannot be passed to `advance_attached_ref`), and
///   `detach_head` takes an `AttachedRef`, so no consent token opens the
///   detaching route either.
///
/// A `detach_checkouts` consent converts both refusals into the detach they
/// name; it never turns a refused MOVE into a performed one, so the branch
/// ref is left where it is in every case.
///
/// Returns the refusal or failure message when the operation must stop.
fn realign_present_clone(
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    dest: &Path,
    target: &ResolvedRevisionId,
    detach_checkouts: Option<DetachConsent>,
) -> Result<(), String> {
    let describe = |e: VcsError| format!("{}: {e:#}", repo_path.as_str());

    let attached = match vcs.head_attachment(dest).map_err(describe)? {
        HeadAttachment::Detached(was) => {
            if was.at() == target {
                // The absorbed no-op: a clone already at the pin is not
                // realigned at all, so nothing is asked of the operator and
                // nothing is written.
                return Ok(());
            }
            return vcs.advance_detached_head(&was, target).map_err(describe);
        }
        HeadAttachment::Unborn(u) => {
            return Err(format!(
                "{}: branch '{u}' has no commits yet — rwv fetch has no way to \
                 place {} on an unborn branch. Make an initial commit, or check \
                 out a branch that has one, then re-run.",
                repo_path.as_str(),
                target.display_str(),
            ));
        }
        HeadAttachment::Attached(a) => a,
    };

    let counterpart = entry.version.local_counterpart();

    if !attached.is_named(&counterpart) {
        let Some(consent) = detach_checkouts else {
            return Err(format!(
                "{}: is on branch '{attached}', not the local counterpart \
                 ('{counterpart}') of the branch the manifest declares — rwv \
                 fetch moves only that branch, so it will not relocate one it \
                 cannot relate to the lock.\n  \
                 Switch to '{counterpart}' and re-run, or re-run with \
                 --detach-checkouts to materialize {} on a detached HEAD \
                 (your branch is not moved).",
                repo_path.as_str(),
                target.display_str(),
            ));
        };
        return vcs
            .detach_head(&attached, target, consent)
            .map_err(describe);
    }

    let head = vcs.head_revision(dest).map_err(describe)?;
    if &head == target {
        return Ok(());
    }
    // Classify before acting rather than reading a failure back out of git:
    // `merge --ff-only` also fails on a dirty tree, and the two need
    // different messages.
    if vcs.is_ancestor(dest, &head, target).map_err(describe)? {
        return vcs
            .advance_attached_ref(&attached, target)
            .map_err(describe);
    }
    let Some(consent) = detach_checkouts else {
        return Err(format!(
            "{}: aligning '{attached}' to {} is not a fast-forward — the pin is \
             not a descendant of the branch tip, which is what materializing an \
             older lock, or a branch carrying commits {} has not seen, looks \
             like.\n  \
             Reconcile '{attached}' with the pin yourself (ordinary `git rebase` \
             / `git merge`) and re-run, or re-run with --detach-checkouts to \
             materialize {} on a detached HEAD (the branch ref is not moved).",
            repo_path.as_str(),
            target.display_str(),
            vcs.conventional_remote_name(),
            target.display_str(),
        ));
    };
    vcs.detach_head(&attached, target, consent)
        .map_err(describe)
}

/// Per-repo worker for `rwv fetch`. Encapsulates one iteration of the
/// previous serial loop body.
///
/// Returns [`FetchOutcome`]. The caller threads `add_to_lock` into the
/// post-join lock-write step and aggregates failures.
///
/// When `output` is a JSON mode, progress lines are emitted to stderr (via
/// `reporter.err`) rather than stdout, so that stdout carries only
/// machine-readable JSON output.
#[allow(clippy::too_many_arguments)]
fn fetch_one(
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    workspace_root: &Path,
    existing_lock: Option<&LockFile>,
    selection: &MemberSelection,
    detach_checkouts: Option<DetachConsent>,
    reporter: &Reporter<'_>,
    output: OutputMode,
) -> FetchOutcome {
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
            "rwv fetch: skipping {} (role: reference)",
            repo_path.as_str()
        ));
        return FetchOutcome::Skipped;
    }

    let dest: PathBuf = workspace_root.join(repo_path.as_path());

    let lock_entry = existing_lock.and_then(|l| l.get_entry(repo_path).cloned());

    if dest.exists() {
        if let Some(lock_entry) = lock_entry {
            emit(&format!(
                "rwv fetch: aligning {} to {}",
                repo_path.as_str(),
                lock_entry.version,
            ));
            let resolved = match vcs.resolve_revision(&dest, lock_entry.version.as_str()) {
                Ok(r) => r,
                Err(e) => {
                    return FetchOutcome::Failed {
                        msg: format!(
                            "{}: failed to resolve {}: {e:#}",
                            repo_path.as_str(),
                            lock_entry.version,
                        ),
                    };
                }
            };
            if let Err(msg) =
                realign_present_clone(vcs, repo_path, entry, &dest, &resolved, detach_checkouts)
            {
                return FetchOutcome::Failed { msg };
            }
            return FetchOutcome::Ok { add_to_lock: None };
        } else if existing_lock.is_some() {
            // Lock exists but doesn't cover this repo — additive add at
            // branch HEAD. The clone already exists; nothing to do
            // beyond marking it for the lock write below.
            emit(&format!(
                "rwv fetch: adding {} to lock at branch HEAD (additive)",
                repo_path.as_str()
            ));
            return FetchOutcome::Ok {
                add_to_lock: Some(repo_path.clone()),
            };
        } else {
            // Bootstrap (no lock yet) — clone is pre-existing, just
            // record it. The lock-write step below will snapshot
            // everything from disk.
            emit(&format!(
                "rwv fetch: skip {} (already exists)",
                repo_path.as_str()
            ));
            return FetchOutcome::Ok { add_to_lock: None };
        }
    }

    let clone_url = match registry::resolve_to_clone_info(&entry.url) {
        Ok(info) => info.url,
        Err(e) => {
            return FetchOutcome::Failed {
                msg: format!("{}: {e:#}", repo_path.as_str()),
            };
        }
    };

    // Create parent directories
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return FetchOutcome::Failed {
                msg: format!(
                    "{}: failed to create directory {}: {e:#}",
                    repo_path.as_str(),
                    parent.display()
                ),
            };
        }
    }

    emit(&format!(
        "rwv fetch: cloning {} from {} (role: {})",
        repo_path.as_str(),
        clone_url,
        entry.role
    ));

    let mut add_to_lock: Option<RepoPath> = None;
    // A lock-pinned clone is born ATTACHED AT THE PIN — one operation, not a
    // clone followed by an align. Cloning to the remote tip and then moving
    // to the pin would leave the member detached, which is not a state a
    // fetch is entitled to put an operator's checkout in.
    if let Some(lock_entry) = lock_entry {
        let counterpart = entry.version.local_counterpart();
        emit(&format!(
            "rwv fetch: cloning {} onto {counterpart} at {}",
            repo_path.as_str(),
            lock_entry.version,
        ));
        if let Err(e) = vcs.clone_attached_at(&clone_url, &dest, &counterpart, &lock_entry.version)
        {
            return FetchOutcome::Failed {
                msg: format!(
                    "{}: failed to materialize {} at {}: {e:#}",
                    repo_path.as_str(),
                    clone_url,
                    lock_entry.version,
                ),
            };
        }
        return FetchOutcome::Ok { add_to_lock };
    }

    if let Err(e) = vcs.clone_repo(&clone_url, &dest) {
        return FetchOutcome::Failed {
            msg: format!(
                "{}: failed to clone {} into {}: {e:#}",
                repo_path.as_str(),
                clone_url,
                dest.display()
            ),
        };
    }

    if existing_lock.is_some() {
        // Lock exists but doesn't cover this repo — leave at branch HEAD
        // (where the clone landed) and mark for additive lock entry. Emit
        // a message so the additive path is observable to the operator —
        // matches the "adding … to lock at branch HEAD" text the
        // dest-already-exists arm above emits.
        emit(&format!(
            "rwv fetch: cloned {} at branch HEAD (additive — no lock entry)",
            repo_path.as_str()
        ));
        add_to_lock = Some(repo_path.clone());
    }
    // else: bootstrap, will be picked up wholesale below.

    FetchOutcome::Ok { add_to_lock }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_source_passes_through_urls() {
        let url = "https://github.com/org/repo.git";
        assert_eq!(resolve_source(url).unwrap().to_string(), url);
    }

    #[test]
    fn resolve_source_passes_through_ssh_urls() {
        let url = "git@github.com:org/repo.git";
        assert_eq!(resolve_source(url).unwrap().to_string(), url);
    }

    #[test]
    fn resolve_source_passes_through_file_urls() {
        let url = format!("file://{}", std::env::temp_dir().join("repo.git").display());
        assert_eq!(resolve_source(&url).unwrap().to_string(), url);
    }

    #[test]
    fn resolve_source_resolves_two_part_shorthand() {
        let url = resolve_source("cwalv/repoweave").unwrap();
        assert_eq!(url.to_string(), "https://github.com/cwalv/repoweave.git");
    }

    #[test]
    fn resolve_source_resolves_three_part_shorthand() {
        let url = resolve_source("gitlab/org/proj").unwrap();
        assert_eq!(url.to_string(), "https://gitlab.com/org/proj.git");
    }

    #[test]
    fn resolve_source_rejects_invalid_shorthand() {
        assert!(resolve_source("not-a-valid-source").is_err());
    }

    #[test]
    fn resolve_source_rejects_four_part_shorthand() {
        assert!(resolve_source("a/b/c/d").is_err());
    }

    // find_incomplete_repos: --no-reference should exempt reference repos

    fn make_entry(role: Role) -> crate::manifest::RepoEntry {
        crate::manifest::RepoEntry {
            vcs_type: crate::manifest::VcsType::Git,
            url: "https://example.com/repo.git".parse().unwrap(),
            version: crate::vcs::TrackingRef::parse(crate::vcs::RawRefName::new("main")).unwrap(),
            role,
            submodules: crate::manifest::Submodules::Local,
        }
    }

    fn selection(no_reference: bool) -> MemberSelection {
        MemberSelection::new(
            RepoFilter::parse(&[], &[]).expect("an empty filter parses"),
            no_reference,
        )
    }

    fn make_lock_entry() -> crate::manifest::LockEntry {
        crate::manifest::LockEntry {
            vcs_type: crate::manifest::VcsType::Git,
            url: "https://example.com/repo.git".parse().unwrap(),
            version: crate::vcs::RawRevisionId::new("abc123"),
        }
    }

    #[test]
    fn find_incomplete_repos_flags_reference_when_no_reference_is_false() {
        let mut manifest = Manifest {
            repositories: Default::default(),
            integrations: Default::default(),
            workweave: None,
        };
        let primary = RepoPath::new("github/org/primary").expect("known-safe literal");
        let reference = RepoPath::new("github/org/reference").expect("known-safe literal");
        manifest
            .repositories
            .insert(primary.clone(), make_entry(Role::Owned));
        manifest
            .repositories
            .insert(reference.clone(), make_entry(Role::Reference));

        // Lock covers only the primary — reference is incomplete.
        let mut lock = LockFile {
            repositories: Default::default(),
        };
        lock.repositories.insert(primary, make_lock_entry());

        let incomplete = find_incomplete_repos(&manifest, &lock, &selection(false));
        assert_eq!(incomplete, vec![reference]);
    }

    #[test]
    fn find_incomplete_repos_excludes_reference_when_no_reference_is_true() {
        let mut manifest = Manifest {
            repositories: Default::default(),
            integrations: Default::default(),
            workweave: None,
        };
        let primary = RepoPath::new("github/org/primary").expect("known-safe literal");
        let reference = RepoPath::new("github/org/reference").expect("known-safe literal");
        manifest
            .repositories
            .insert(primary.clone(), make_entry(Role::Owned));
        manifest
            .repositories
            .insert(reference, make_entry(Role::Reference));

        let mut lock = LockFile {
            repositories: Default::default(),
        };
        lock.repositories.insert(primary, make_lock_entry());

        // With no_reference=true, the missing reference entry is not flagged.
        let incomplete = find_incomplete_repos(&manifest, &lock, &selection(true));
        assert!(incomplete.is_empty(), "expected empty, got {incomplete:?}");
    }

    // FetchMode enum tests

    #[test]
    fn fetch_mode_variants_are_distinct() {
        assert_ne!(FetchMode::Default, FetchMode::Frozen);
    }

    #[test]
    fn fetch_mode_default_is_default_variant() {
        // The default mode (no flags) should be FetchMode::Default.
        let mode = FetchMode::Default;
        assert_eq!(mode, FetchMode::Default);
    }

    #[test]
    fn fetch_mode_is_copy() {
        // FetchMode should be Copy — it's a simple enum.
        let a = FetchMode::Default;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn fetch_mode_debug() {
        // Verify Debug is derived (used in error messages).
        let s = format!("{:?}", FetchMode::Frozen);
        assert!(s.contains("Frozen"));
    }
}
