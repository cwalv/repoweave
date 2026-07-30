//! `rwv fetch` — clone a project and its repos into the workspace.
//!
//! The source must be a URL (full URL or `owner/repo` shorthand resolved via
//! the registry). Local paths are not accepted; use `rwv activate` instead.

use crate::cli::consent::DetachConsent;
use crate::lock;
use crate::manifest::{LockFile, Manifest, RepoEntry, RepoPath, Role};
use crate::parallel::{run_in_parallel, Reporter};
use crate::registry;
use crate::selector::RepoFilter;
use crate::vcs::{
    project_vcs, vcs_for, HeadAttachment, RawRefName, ResolvedRevisionId, TrackingRef, Vcs,
    VcsError,
};
use crate::workspace::{Resolution, WorkspaceContext};
use anyhow::{bail, Context};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Schema URL embedded in `rwv fetch --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/fetch.json`.
pub const FETCH_SCHEMA_URL: &str = crate::schema_url::schema_url!("fetch");

/// Per-repo outcome record for `rwv fetch --json`.
///
/// `status` is one of `"ok"`, `"skipped"`, or `"failed"`. `message` carries
/// a human-readable description of the outcome (always present for `"failed"`;
/// present for `"skipped"` to say why; `null` for `"ok"`).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchOutcomeOutput {
    pub path: String,
    pub absolute_path: String,
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
#[serde(rename_all = "snake_case")]
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
    /// Resolved workspace coordinates (workspace root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// One NDJSON record emitted by `rwv fetch --json -j N` with `N > 1`.
///
/// Each per-repo outcome becomes its own self-describing line. Every record
/// carries its own `$schema` URL so consumers can identify a line without
/// out-of-band context. Serialised with `#[serde(flatten)]` so the wire shape
/// is a single flat object: `{"$schema": "...", "path": "...", ...}`.
#[derive(Debug, Serialize)]
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
    /// additively. Auto-activates the project on first fetch.
    Default,
    /// Like Default, but error when the lock is absent or incomplete
    /// rather than writing it. Clones are aligned the same way.
    Frozen,
}

/// Resolve `source` to a clone URL and the owner string.
///
/// Accepts full URLs (returned as-is) or `owner/repo` / `registry/owner/repo`
/// shorthand (resolved via the built-in registries to an HTTPS clone URL).
///
/// Returns `(url, owner)` where `owner` may be empty for unrecognised URLs.
fn resolve_source(source: &str) -> anyhow::Result<(crate::manifest::RepoUrl, String)> {
    let parsed: crate::manifest::RepoUrl = source.parse()?;
    let info = registry::resolve_to_clone_info(&parsed)?;
    let owner = info.id.owner().to_owned();
    Ok((info.url, owner))
}

/// Validate that a lock file covers all repos in the manifest.
///
/// Returns a list of repo paths present in the manifest but missing from the lock.
/// When `no_reference` is set, `reference`-role repos are excluded from the
/// check — the user has opted out of fetching them, so missing lock entries
/// for them shouldn't fail `--frozen`.
fn find_incomplete_repos(
    manifest: &Manifest,
    lock: &LockFile,
    no_reference: bool,
) -> Vec<RepoPath> {
    manifest
        .iter_entries()
        .filter(|(_, entry)| !(no_reference && entry.role == Role::Reference))
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
/// When `json` is `true`:
/// - `jobs == 1` (or no `-j`): emits a single `{ "$schema": ..., "outcomes":
///   [...] }` envelope to stdout after all repos complete.
/// - `jobs > 1`: streams one self-describing NDJSON line per repo to stdout
///   as each worker finishes, then emits no envelope wrapper.
#[allow(clippy::too_many_arguments)]
pub fn run_fetch(
    source: &str,
    workspace_root: &Path,
    mode: FetchMode,
    no_reference: bool,
    detach_checkouts: Option<DetachConsent>,
    filter: &RepoFilter,
    jobs: usize,
    json: bool,
) -> anyhow::Result<()> {
    let project_vcs = project_vcs();

    // Resolve source to a clone URL (supports full URLs and owner/repo shorthand).
    let (url, owner) = resolve_source(source)?;
    let url_str = url.to_string();
    let name = registry::repo_name_from_source(&url_str);
    let projects_dir = workspace_root.join("projects");
    std::fs::create_dir_all(&projects_dir).context("failed to create projects/ directory")?;
    let project_dir = projects_dir.join(&name);
    if project_dir.exists() {
        // Project name already taken — surface a helpful scoped-path hint.
        let scoped = if owner.is_empty() {
            format!("projects/{{owner}}/{name}/")
        } else {
            format!("projects/{owner}/{name}/")
        };
        eprintln!("Error: project '{name}' already exists at projects/{name}/");
        eprintln!("Hint: try a scoped path: {scoped}");
        bail!("project '{}' already exists at projects/{}/", name, name);
    } else {
        // In JSON mode, project-level progress goes to stderr so stdout stays
        // JSON-only. In text mode it goes to stdout as before.
        if json {
            eprintln!("rwv fetch: cloning project '{}'", name);
        } else {
            println!("rwv fetch: cloning project '{}'", name);
        }
        project_vcs
            .clone_repo(&url_str, &project_dir)
            .with_context(|| format!("failed to clone project source '{}'", url))?;
    }

    fetch_project_repos(
        &name,
        &project_dir,
        workspace_root,
        mode,
        no_reference,
        detach_checkouts,
        filter,
        jobs,
        json,
        /* auto_activate_on_bootstrap = */ true,
        // Source-mode fetch runs before any WorkspaceContext is resolved; no
        // resolution block is available at this point.
        None,
    )
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
/// lock covers it, [`fetch_one`] resolves the locked revision in that clone's
/// own object store and [`realign_present_clone`] advances the checkout to it
/// — fast-forwarding the tracking branch's local counterpart, or refusing
/// unless `detach_checkouts` carries a consent. When the lock has no entry for
/// the repo, or there is no lock, the clone is left alone and the lock records
/// its current HEAD.
///
/// Clone-topology (I1): the canonical clone always lives at primary's
/// `<weave>/<repo_path>`, even when this verb is invoked from inside a
/// workweave. In-place fetch therefore materializes into
/// [`WorkspaceContext::primary_path`]; the workweave will pick up the newly
/// available canonical via `rwv sync` (which does the worktree-add) — this
/// matches how `rwv add` and `sync::materialize_missing_repo` divide labor.
///
/// The auto-activate step from the SOURCE-mode path is not repeated: in-place
/// mode operates on an already-active project.
///
/// Flag semantics: `--frozen`, `--no-reference`, `--role`/`--repo`, `-j`,
/// `--json` all carry the same meaning as the SOURCE-mode path. Filtered
/// runs are additive and skip the lock-write step (same rule as bootstrap).
#[allow(clippy::too_many_arguments)]
pub fn run_fetch_in_place(
    ctx: &WorkspaceContext,
    mode: FetchMode,
    no_reference: bool,
    detach_checkouts: Option<DetachConsent>,
    filter: &RepoFilter,
    jobs: usize,
    json: bool,
) -> anyhow::Result<()> {
    let name = ctx.require_active_project_on_disk()?.clone();

    // Per-workspace state (rwv.yaml, rwv.lock) lives under active_path — the
    // workweave when in one, primary otherwise. Mirrors add_remove.rs's
    // find_project_dir.
    let project_dir = ctx.active_path().join("projects").join(name.as_str());

    // Clone destination (canonical store) is always primary's slot — clone-
    // topology I1. Passing primary_path() as workspace_root routes fetch_one
    // to write clones at `<primary>/<repo_path>/`, even when CWD is inside a
    // workweave. See docs/explanation/joints/clone-topology.md.
    let workspace_root = ctx.primary_path();

    fetch_project_repos(
        name.as_str(),
        &project_dir,
        workspace_root,
        mode,
        no_reference,
        detach_checkouts,
        filter,
        jobs,
        json,
        /* auto_activate_on_bootstrap = */ false,
        ctx.resolution(),
    )
}

/// Shared per-project repo-materialization loop for both SOURCE-mode
/// (bootstrap: `rwv fetch <source>`) and in-place mode (`rwv fetch` with no
/// SOURCE). Both entries route through this helper so the clone/checkout,
/// lock-alignment, and lock-write behavior stays identical across both
/// invocations.
///
/// `auto_activate_on_bootstrap` toggles the first-fetch auto-activate step
/// (only meaningful for SOURCE-mode — the in-place path operates on a
/// project that is already active).
///
/// `resolution` is the resolved workspace coordinate block for the
/// `--json` envelope. `None` when no `WorkspaceContext` is available (the
/// SOURCE-mode bootstrap path runs before a context is resolved).
#[allow(clippy::too_many_arguments)]
fn fetch_project_repos(
    name: &str,
    project_dir: &Path,
    workspace_root: &Path,
    mode: FetchMode,
    no_reference: bool,
    detach_checkouts: Option<DetachConsent>,
    filter: &RepoFilter,
    jobs: usize,
    json: bool,
    auto_activate_on_bootstrap: bool,
    resolution: Option<Resolution>,
) -> anyhow::Result<()> {
    // Read the manifest
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to read manifest from {}", manifest_path.display()))?;

    // Load the lock file. In Frozen mode the lock must exist and cover all
    // manifest repos. In Default mode the lock may be absent (bootstrap), and
    // missing repos are added additively after fetch.
    let lock_path = project_dir.join(LockFile::FILE_NAME);
    let existing_lock: Option<LockFile> = if lock_path.exists() {
        Some(
            LockFile::from_path(&lock_path)
                .with_context(|| format!("failed to read lock file at {}", lock_path.display()))?,
        )
    } else {
        None
    };

    match mode {
        FetchMode::Frozen => {
            let lock = existing_lock.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "rwv fetch --frozen: lock file does not exist at {}",
                    lock_path.display()
                )
            })?;
            let missing = find_incomplete_repos(&manifest, lock, no_reference);
            if !missing.is_empty() {
                let names: Vec<&str> = missing.iter().map(|rp| rp.as_str()).collect();
                bail!(
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
    }

    // Snapshot the work list. BTreeMap iteration is deterministic, so the
    // resulting Vec preserves the serial loop's ordering (acceptance: -j 1
    // matches the previous behaviour exactly).
    //
    // Apply the `--role` / `--repo` filter here so the worker pool only sees
    // selected repos. Empty filter is a no-op (every repo passes). See
    // `src/selector.rs` for the grammar and union semantics.
    let work_items: Vec<(RepoPath, RepoEntry, Box<dyn Vcs>)> = manifest
        .iter_entries()
        .filter(|(rp, entry)| filter.matches(rp, entry.role))
        .map(|(rp, e)| (rp.clone(), e.clone(), vcs_for(e.vcs_type)))
        .collect();

    let parallel = jobs > 1;
    let write_lock: Mutex<()> = Mutex::new(());

    let ndjson = crate::parallel::OutputMode::resolve(json, jobs).is_ndjson();

    let outcomes: Vec<FetchOutcome> = run_in_parallel(&work_items, jobs, |_idx, item| {
        let (repo_path, entry, vcs) = item;
        let reporter = if parallel && !json {
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
        let outcome = fetch_one(
            vcs.as_ref(),
            repo_path,
            entry,
            workspace_root,
            existing_lock.as_ref(),
            no_reference,
            detach_checkouts,
            &reporter,
            json,
        );

        // NDJSON mode: emit one line per repo as soon as it finishes.
        if ndjson {
            let abs_path = workspace_root
                .join(repo_path.as_path())
                .to_string_lossy()
                .into_owned();
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
                schema: FETCH_SCHEMA_URL,
                outcome: &record,
            };
            if let Ok(line) = serde_json::to_string(&ndjson_line) {
                let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                let stdout = std::io::stdout();
                let mut handle = stdout.lock();
                let _ = writeln!(handle, "{line}");
                let _ = handle.flush();
            }
        }

        outcome
    });

    // Aggregate outcomes serially in input order — preserves the existing
    // error-report shape and gives the lock-write step a deterministic
    // `added_to_lock` list.
    let mut succeeded = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut added_to_lock: Vec<RepoPath> = Vec::new();

    // For envelope mode (json && !ndjson), collect records in work_items order.
    let mut envelope_records: Vec<FetchOutcomeOutput> = Vec::new();

    for (outcome, (repo_path, _entry, _vcs)) in outcomes.iter().zip(work_items.iter()) {
        let abs_path = workspace_root
            .join(repo_path.as_path())
            .to_string_lossy()
            .into_owned();
        match outcome {
            FetchOutcome::Ok { add_to_lock } => {
                succeeded += 1;
                if let Some(rp) = add_to_lock {
                    added_to_lock.push(rp.clone());
                }
                if json && !ndjson {
                    envelope_records.push(FetchOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: FetchOutcomeStatus::Ok,
                        message: None,
                    });
                }
            }
            FetchOutcome::Skipped => {
                if json && !ndjson {
                    envelope_records.push(FetchOutcomeOutput {
                        path: repo_path.to_string(),
                        absolute_path: abs_path,
                        status: FetchOutcomeStatus::Skipped,
                        message: Some(format!("skipped {}", repo_path.as_str())),
                    });
                }
            }
            FetchOutcome::Failed { msg } => {
                if !json {
                    eprintln!("rwv fetch: error: {msg}");
                }
                errors.push(msg.clone());
                if json && !ndjson {
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
        if json {
            // In JSON mode, emit the envelope/NDJSON before exiting so
            // consumers always get parseable output even on partial failure.
            if !ndjson {
                let payload = FetchJsonOutput {
                    schema: FETCH_SCHEMA_URL.to_owned(),
                    outcomes: envelope_records,
                    resolution: resolution.clone(),
                };
                if let Ok(out) = serde_json::to_string_pretty(&payload) {
                    println!("{out}");
                }
            }
            // NDJSON records were already streamed; nothing extra to emit.
            bail!(
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
        bail!(
            "fetch completed with {} clone failure(s) out of {total} repo(s)",
            errors.len()
        )
    }

    if json {
        if !ndjson {
            // Envelope mode: emit after all repos complete.
            let payload = FetchJsonOutput {
                schema: FETCH_SCHEMA_URL.to_owned(),
                outcomes: envelope_records,
                resolution,
            };
            let out = serde_json::to_string_pretty(&payload)
                .context("failed to serialize fetch output")?;
            println!("{out}");
        }
        // NDJSON mode: all records streamed by workers; nothing more to emit.
    } else {
        println!("rwv fetch: done ({succeeded} repo(s) ready)");
    }

    // Default mode: bootstrap or additively extend the lock; then maybe auto-activate.
    //
    // A non-empty `--role` / `--repo` filter narrows the fetch to a subset of
    // the manifest; non-filtered repos are not on disk (in a fresh workspace)
    // or have not been touched (in an existing workspace). Either way, the
    // lock-write paths below — both bootstrap and additive — assume they can
    // read HEAD from every manifest repo; under a filter that's false. Skip
    // the lock-write entirely under a filter: the filtered fetch is a
    // targeted-sync operation, not a lock-bumping one. Use `rwv update` or
    // unfiltered `rwv fetch` to refresh the lock.
    if mode == FetchMode::Default && filter.is_empty() {
        let needs_bootstrap = existing_lock.is_none();
        let has_additions = !added_to_lock.is_empty();

        // generate_lock walks every entry in the manifest and runs
        // `git rev-parse HEAD` against each on-disk repo. When --no-reference
        // is set, reference repos were skipped above and their directories
        // don't exist on disk — drop them from the manifest used for lock
        // generation so we don't trip on the missing paths.
        let lock_manifest = if no_reference {
            let mut filtered = manifest.clone();
            filtered.retain_repos(|_, entry| entry.role != Role::Reference);
            std::borrow::Cow::Owned(filtered)
        } else {
            std::borrow::Cow::Borrowed(&manifest)
        };

        if needs_bootstrap {
            // Snapshot the full set of manifest repos from disk.
            let new_lock = lock::generate_lock(&lock_manifest, workspace_root, None, true)?;
            lock::write_lock(&new_lock, &lock_path)?;
            eprintln!("rwv fetch: wrote {}", lock_path.display());
        } else if has_additions {
            // Preserve existing lock entries as written (do not re-resolve
            // — that could rewrite tag-form versions as raw SHAs). Append
            // new entries by snapshotting HEAD for the added repos.
            let mut merged = existing_lock
                .as_ref()
                .expect("existing_lock is Some when !needs_bootstrap")
                .clone();
            // Generate a fresh lock for new entries only.
            let new_lock = lock::generate_lock(&lock_manifest, workspace_root, None, true)?;
            for repo_path in &added_to_lock {
                if let Some(entry) = new_lock.get_entry(repo_path) {
                    merged.insert_entry(repo_path.clone(), entry.to_raw());
                }
            }
            lock::write_lock(&merged, &lock_path)?;
            eprintln!("rwv fetch: wrote {}", lock_path.display());
        }

        // Auto-activate only when no project is already active (first fetch).
        // In-place mode always skips auto-activate (the project is already
        // active — that's how the in-place caller resolved the manifest).
        if auto_activate_on_bootstrap {
            match crate::workspace::read_active_project(workspace_root) {
                Some(active) => {
                    // Route to stderr in JSON mode so stdout stays JSON-only.
                    let msg = format!(
                        "rwv fetch: skipping auto-activate (project '{active}' already active)"
                    );
                    if json {
                        eprintln!("{msg}");
                    } else {
                        println!("{msg}");
                    }
                }
                None => {
                    // Resolve the freshly-bootstrapped workspace to build the
                    // context activate now takes. This is a first-resolution of
                    // the newly-created workspace, not a re-resolution of the
                    // invocation context.
                    let ctx = crate::workspace::WorkspaceContext::resolve(workspace_root, None)?;
                    crate::activate::activate(name, &ctx)?;
                }
            }
        }
    }

    Ok(())
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
    let describe = |e: VcsError| format!("{}: {e}", repo_path.as_str());

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

    // `entry.version` is the manifest's declared tracking branch, still typed
    // `RefName` (manifest.rs's migration to `TrackingRef` is separate work).
    // Route it through `TrackingRef::parse` so the comparison below goes
    // through `local_counterpart()` — the same named projection `rwv push`'s
    // gates use — instead of a raw string compare.
    let declared = TrackingRef::parse(RawRefName::new(entry.version.as_str())).map_err(|e| {
        format!(
            "{}: manifest declares an invalid tracking branch '{}': {e}",
            repo_path.as_str(),
            entry.version,
        )
    })?;
    let counterpart = declared.local_counterpart();

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
             older lock, or a branch carrying commits origin has not seen, looks \
             like.\n  \
             Reconcile '{attached}' with the pin yourself (ordinary `git rebase` \
             / `git merge`) and re-run, or re-run with --detach-checkouts to \
             materialize {} on a detached HEAD (the branch ref is not moved).",
            repo_path.as_str(),
            target.display_str(),
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
/// When `json` is `true`, progress lines are emitted to stderr (via
/// `reporter.err`) rather than stdout, so that stdout carries only
/// machine-readable JSON output.
#[allow(clippy::too_many_arguments)]
fn fetch_one(
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    workspace_root: &Path,
    existing_lock: Option<&LockFile>,
    no_reference: bool,
    detach_checkouts: Option<DetachConsent>,
    reporter: &Reporter<'_>,
    json: bool,
) -> FetchOutcome {
    // Helper: route progress to stdout (text mode) or stderr (JSON mode).
    let emit = |msg: &str| {
        if json {
            reporter.err(msg);
        } else {
            reporter.out(msg);
        }
    };

    if no_reference && entry.role == Role::Reference {
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
                            "{}: failed to resolve {}: {e}",
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

    // Create parent directories
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return FetchOutcome::Failed {
                msg: format!(
                    "{}: failed to create directory {}: {e}",
                    repo_path.as_str(),
                    parent.display()
                ),
            };
        }
    }

    emit(&format!(
        "rwv fetch: cloning {} from {} (role: {})",
        repo_path.as_str(),
        entry.url,
        entry.role.as_str()
    ));

    let mut add_to_lock: Option<RepoPath> = None;
    // A lock-pinned clone is born ATTACHED AT THE PIN — one operation, not a
    // clone followed by an align. Cloning to the remote tip and then moving
    // to the pin would leave the member detached, which is not a state a
    // fetch is entitled to put an operator's checkout in.
    if let Some(lock_entry) = lock_entry {
        let declared = match TrackingRef::parse(RawRefName::new(entry.version.as_str())) {
            Ok(t) => t,
            Err(e) => {
                return FetchOutcome::Failed {
                    msg: format!(
                        "{}: manifest declares an invalid tracking branch '{}': {e}",
                        repo_path.as_str(),
                        entry.version,
                    ),
                };
            }
        };
        emit(&format!(
            "rwv fetch: cloning {} onto {} at {}",
            repo_path.as_str(),
            declared.local_counterpart(),
            lock_entry.version,
        ));
        if let Err(e) = vcs.clone_attached_at(
            &entry.url.to_string(),
            &dest,
            entry.role,
            &declared.local_counterpart(),
            &lock_entry.version,
        ) {
            return FetchOutcome::Failed {
                msg: format!(
                    "{}: failed to materialize {} at {}: {e}",
                    repo_path.as_str(),
                    entry.url,
                    lock_entry.version,
                ),
            };
        }
        return FetchOutcome::Ok { add_to_lock };
    }

    if let Err(e) = vcs.clone_with_role(&entry.url.to_string(), &dest, entry.role) {
        return FetchOutcome::Failed {
            msg: format!(
                "{}: failed to clone {} into {}: {e}",
                repo_path.as_str(),
                entry.url,
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
        let (resolved_url, owner) = resolve_source(url).unwrap();
        assert_eq!(resolved_url.to_string(), url);
        assert_eq!(owner, "org");
    }

    #[test]
    fn resolve_source_passes_through_ssh_urls() {
        let url = "git@github.com:org/repo.git";
        let (resolved_url, owner) = resolve_source(url).unwrap();
        assert_eq!(resolved_url.to_string(), url);
        assert_eq!(owner, "org");
    }

    #[test]
    fn resolve_source_passes_through_file_urls() {
        let url = format!("file://{}", std::env::temp_dir().join("repo.git").display());
        let (resolved_url, owner) = resolve_source(&url).unwrap();
        assert_eq!(resolved_url.to_string(), url);
        // file:// URLs that don't match any registry have an empty owner
        let _ = owner; // owner may be empty or a path segment; just verify no panic
    }

    #[test]
    fn resolve_source_resolves_two_part_shorthand() {
        let (url, owner) = resolve_source("cwalv/repoweave").unwrap();
        assert_eq!(url.to_string(), "https://github.com/cwalv/repoweave.git");
        assert_eq!(owner, "cwalv");
    }

    #[test]
    fn resolve_source_resolves_three_part_shorthand() {
        let (url, owner) = resolve_source("gitlab/org/proj").unwrap();
        assert_eq!(url.to_string(), "https://gitlab.com/org/proj.git");
        assert_eq!(owner, "org");
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
            version: crate::vcs::RefName::new("main"),
            role,
        }
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
            workweave: None,
            repositories: Default::default(),
        };
        lock.repositories.insert(primary, make_lock_entry());

        let incomplete = find_incomplete_repos(&manifest, &lock, false);
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
            workweave: None,
            repositories: Default::default(),
        };
        lock.repositories.insert(primary, make_lock_entry());

        // With no_reference=true, the missing reference entry is not flagged.
        let incomplete = find_incomplete_repos(&manifest, &lock, true);
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
