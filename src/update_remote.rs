//! `rwv update --remote` — advance each manifest repo to its branch HEAD and
//! record the advanced tips in `rwv.lock`.
//!
//! Semantically analogous to `cargo update` or `npm update`: this is the
//! form that mutates the lock by pulling fresh tips from the network.
//! `rwv update` aligns clones to the lock; `rwv lock` records local tips;
//! `rwv update --remote` advances and records. A member with no pin takes
//! its tip as its first pin; a tip that would move a pin backward is refused
//! before any member moves.

use crate::lock;
use crate::manifest::RepoKey;
use crate::manifest::{Project, ProjectName, RepoEntry, RepoPath};
use crate::merge_upstream::{Flags, Op};
use crate::op_state::{OpVerb, TakeInRecord};
use crate::parallel::{run_in_parallel, OutputMode, Reporter};
use crate::placement::PlacementOutput;
use crate::project_merge::{conflict_stop, merge_member, MemberStop, Order};
use crate::selector::RepoFilter;
use crate::vcs::{
    vcs_for, AttachedRef, HeadAttachment, LocalRefName, RawRefName, Relation, ResolvedRevisionId,
    Vcs, VcsError,
};
use crate::workspace::{
    member_checkout_dir, Checkout, MemberCheckout, Resolution, WorkspaceContext,
};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Schema URL for `rwv update --remote --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/update-remote.json`.
pub const UPDATE_REMOTE_SCHEMA_URL: &str = crate::schema_url::schema_url!("update-remote");

/// Per-repo outcome kind for `rwv update --remote --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateKind {
    /// Repo was advanced to a new SHA (old_sha != new_sha).
    Updated,
    /// Repo was already at the branch HEAD (old_sha == new_sha).
    UpToDate,
    /// Advance failed; see `error` for the message.
    Failed,
}

/// Per-repo record in `rwv update --remote --json` output.
///
/// `old_sha` is the tip before the fetch; `new_sha` is the tip after
/// checkout (the new branch HEAD). Both are `null` when the SHA could not
/// be read (e.g. the repo was missing from disk before the advance). For
/// `kind = failed`, `new_sha` is always `null`; `error` carries the
/// human-readable failure message.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RepoUpdateRecord {
    /// Manifest-relative path.
    pub path: String,
    /// Fully resolved absolute path.
    pub absolute_path: crate::path_spelling::WirePath,
    /// Branch name from the manifest `branch` field.
    pub branch: String,
    /// Outcome discriminant.
    pub kind: UpdateKind,
    /// Tip SHA before the advance (`null` if unreadable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_sha: Option<String>,
    /// Tip SHA after the advance (`null` when `kind = failed`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_sha: Option<String>,
    /// Human-readable error message, only present when `kind = failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Top-level envelope for `rwv update --remote --json`.
/// `{ "$schema": "<url>", "repos": [<RepoUpdateRecord>, ...] }`
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateRemoteJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub repos: Vec<RepoUpdateRecord>,
    /// How the files of each checkout the advance moved were placed. Empty,
    /// not absent, when it moved none.
    pub placements: Vec<PlacementOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// The materialization side effects this update skipped, each naming the
    /// command that performs it. Empty, not absent, when it skipped none.
    #[serde(default)]
    pub skipped: Vec<crate::skipped::SkippedEffect>,
}

/// Run `rwv update --remote` for the current workspace context.
///
/// For each repo in the active project's manifest:
/// 1. `git fetch` the remote.
/// 2. Resolve the branch (`branch` in the manifest) on the remote — this
///    is the canonical "HEAD of the upstream branch" value.
/// 3. Checkout that revision in the local clone.
///
/// After all repos are advanced, record the tips of the members the filter
/// selects in `rwv.lock`, every other pin as it was; `dirty` controls whether
/// to bypass the uncommitted-changes check of the members recorded.
///
/// The enabled integrations then re-author their managed/generated content
/// against the advanced tree, so it can be committed alongside the lock. This
/// step is skipped when `filter` is non-empty — see the guard at the call
/// site.
///
/// When `commit` is set, the lock and the content authored against the
/// tips it records are staged and committed together in the project repo —
/// the derived change is never separated from the change that caused it.
/// When `project_override` is `Some`, that project is updated instead of
/// the active one (one-shot; does not change `.rwv-active`).
/// Under `--json`, one envelope reaches stdout, its records sorted by repo path.
///
/// `jobs` is the resolved worker count (post-[`crate::parallel::resolve_jobs`]).
/// `jobs == 1` runs serially with no prefix; `jobs > 1` runs the per-repo
/// loop on a bounded worker pool, prefixing stdout/stderr lines with the
/// repo path. The lock write happens serially after all workers join.
#[allow(clippy::too_many_arguments)]
pub fn run_update_remote(
    ctx: &WorkspaceContext,
    dirty: bool,
    commit: Option<lock::CommitMessage>,
    output: OutputMode,
    selection: &Selection,
    jobs: usize,
    opts: crate::activate::ActivateOptions,
    take: Take,
) -> anyhow::Result<()> {
    let resumed = match &take {
        Take::Continue => Some(Op::resume(ctx, OpVerb::UpdateRemote)?),
        Take::Fresh(..) => {
            // Only reads the op-state; the sync engine takes its own exclusion.
            crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
            None
        }
    };
    let record_selection;
    let (dirty, commit, selection, opts, state) = match (take, resumed) {
        (Take::Fresh(flags, wire), None) => (
            dirty,
            commit,
            selection,
            opts,
            State {
                flags,
                wire,
                dirty,
                committing: None,
                resumed: None,
            },
        ),
        (_, Some((op, _, record))) => {
            record_selection = Selection::parse(&record.roles, &record.repos)?;
            (
                record.dirty,
                record
                    .commit
                    .then(|| lock::CommitMessage::from_flag(record.message.clone())),
                &record_selection,
                crate::activate::ActivateOptions {
                    no_materialize: record.no_materialize,
                },
                State {
                    flags: Flags::default().recorded(&record),
                    wire: Wire::default(),
                    dirty: record.dirty,
                    committing: None,
                    resumed: Some((op, record)),
                },
            )
        }
        (Take::Continue, None) => unreachable!("a continue resumes an op"),
    };
    let state = State {
        committing: commit.as_ref().map(|message| match message {
            lock::CommitMessage::Generated => None,
            lock::CommitMessage::Given(text) => Some(text.clone()),
        }),
        ..state
    };

    let (project_name, workweave_dir) = match &ctx.checkout {
        Checkout::Primary { .. } => {
            let name = ctx.require_active_project_on_disk()?.clone();
            (name, None)
        }
        Checkout::Workweave { dir, project, .. } => {
            (project.clone(), Some(dir.as_path().to_path_buf()))
        }
    };

    update_for_project(
        ctx,
        &project_name,
        workweave_dir.as_deref(),
        dirty,
        commit,
        output,
        selection,
        jobs,
        opts,
        &state,
    )
}

/// What `update --remote` was asked to do for a diverged branch.
pub enum Take {
    Fresh(Flags, Wire),
    Continue,
}

/// The selection and the options a resumed op re-applies, spelled as given.
#[derive(Debug, Clone, Default)]
pub struct Wire {
    pub roles: Vec<String>,
    pub repos: Vec<String>,
    pub no_materialize: bool,
}

struct State {
    flags: Flags,
    wire: Wire,
    dirty: bool,
    committing: Option<Option<String>>,
    resumed: Option<(Op, TakeInRecord)>,
}

/// Outcome of advancing a single repo.
///
/// `Ok((new_sha, files))` means fetch + resolve + checkout ran cleanly, the
/// repo is now at `new_sha`, and `files` says how the files it rewrote were
/// placed. `Err(msg)` means one of those steps failed and `msg` is the
/// human-readable failure to surface in the aggregated summary.
type RepoOutcome = Result<(String, Option<crate::share::FilePlacement>), String>;

/// The tip fetched for a member's branch; `None` where the branch has no
/// upstream to take in from.
type Fetched = Result<Option<ResolvedRevisionId>, String>;

/// Extended work item: repo path + manifest entry + old tip SHA captured
/// before the advance loop runs.
/// One branch of a member other than the one this workspace expects, named
/// `<m>:<branch>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchOperand {
    pub repo: RepoPath,
    pub branch: RawRefName,
}

impl std::fmt::Display for BranchOperand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.repo, self.branch)
    }
}

/// What `--role` and `--repo` select: the members fetched and recorded, and
/// the branches named `<m>:<branch>`. A member named only that way has its
/// expected branch left alone.
pub struct Selection {
    pub filter: RepoFilter,
    bare: RepoFilter,
    pub branches: Vec<BranchOperand>,
}

impl Selection {
    pub fn parse(roles: &[String], repos: &[String]) -> anyhow::Result<Self> {
        let mut bare: Vec<String> = Vec::new();
        let mut branches: Vec<BranchOperand> = Vec::new();
        for raw in repos {
            let pattern = raw
                .strip_prefix("glob:")
                .or_else(|| raw.strip_prefix("re:"));
            if let Some(pattern) = pattern {
                if pattern.contains(':') {
                    crate::refuse!(
                        crate::refusal::RefusalKind::InapplicableFlag,
                        "`--repo {raw}`: a `glob:` or `re:` pattern selects repos, never one \
                         branch of a repo, and no repo path holds `:`; name the member by its \
                         path, `--repo <m>:<branch>`"
                    );
                }
                bare.push(raw.clone());
                continue;
            }
            let Some((_, branch)) = raw.split_once(':') else {
                bare.push(raw.clone());
                continue;
            };
            let named = raw
                .parse::<crate::manifest::ResetOperand>()
                .ok()
                .and_then(|named| match named.repo {
                    crate::manifest::RepoOperand::Member(path) => Some(path),
                    crate::manifest::RepoOperand::Project(_) => None,
                });
            let Some(path) = named else {
                crate::refuse!(
                    crate::refusal::RefusalKind::InapplicableFlag,
                    "`--repo {raw}` names no member's branch: `<m>:<branch>` takes a member's \
                     workspace-relative path"
                );
            };
            branches.push(BranchOperand {
                repo: path,
                branch: RawRefName::new(branch.to_owned()),
            });
        }
        let mut selecting = bare.clone();
        selecting.extend(branches.iter().map(|named| named.repo.as_str().to_owned()));
        Ok(Self {
            filter: RepoFilter::parse(roles, &selecting)?,
            bare: RepoFilter::parse(roles, &bare)?,
            branches,
        })
    }

    /// The member is selected only through a `<m>:<branch>` operand.
    fn branch_only(&self, path: &RepoPath, role: crate::manifest::Role) -> bool {
        self.branches.iter().any(|named| named.repo == *path)
            && (self.bare.is_empty() || !self.bare.matches(path, role))
    }
}

struct WorkItem {
    repo_path: RepoPath,
    entry: RepoEntry,
    checkout: MemberCheckout,
    /// HEAD before the advance (`None` if unreadable — e.g. repo missing or
    /// git error).
    old: Option<ResolvedRevisionId>,
    /// The backend resolved from this entry's `vcs_type`, resolved once here
    /// and borrowed by the worker rather than re-resolved per repo.
    vcs: Box<dyn Vcs>,
    /// The branch the member sits on in this workspace: in a feature, an
    /// `owned` or `fork` member's feature branch, else its declared branch.
    expected: LocalRefName,
    /// `expected` is a feature's own branch, advanced from its own upstream.
    feature: bool,
    /// The member's url remote, and the one still to add before the fetch;
    /// `None` where the checkout is missing or the url resolves to no remote.
    planned: Option<crate::url_remote::Planned>,
}

/// Internal: do the update for a specific project under `ctx.active_path()`.
#[allow(clippy::too_many_arguments)]
fn update_for_project(
    ctx: &WorkspaceContext,
    project_name: &ProjectName,
    workweave_dir: Option<&Path>,
    dirty: bool,
    commit: Option<lock::CommitMessage>,
    output: OutputMode,
    selection: &Selection,
    jobs: usize,
    opts: crate::activate::ActivateOptions,
    state: &State,
) -> anyhow::Result<()> {
    let filter = &selection.filter;
    let active_root = ctx.active_path();
    let primary_root = ctx.primary_path();
    let project_dir = crate::workspace::project_dir(active_root, project_name);
    crate::diagnosis::settle_project_drift(project_name, &project_dir)?;
    let project = Project::from_dir(&project_dir, project_name.clone())
        .with_context(|| format!("failed to load project '{}'", project_name))?;
    crate::branch::require_recorded(ctx)?;
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        crate::shared_members::refuse_moving(primary_root, project_name, &|path, role| {
            filter.matches(path, role)
        })?;
    }
    let aligner = crate::alignment::Aligner::open(
        active_root,
        project_name,
        crate::alignment::Accounting::default(),
    )?;
    lock::refuse_detached(&aligner, project_name)?;

    // Snapshot the repo list into a Vec so the parallel loop can index by
    // position. The BTreeMap iteration is deterministic, so the resulting
    // Vec mirrors the previous serial loop's order exactly.
    //
    // Apply the `--role` / `--repo` filter so only selected repos are
    // advanced and recorded. Empty filter is a no-op (every repo passes).
    //
    // Capture the old SHA before the advance so JSON output can report
    // the before/after delta. Missing-repo clones produce `old_sha = None`.
    let work_items: Vec<WorkItem> = project
        .manifest
        .iter_entries()
        .filter(|(rp, entry)| filter.matches(rp, entry.role))
        .map(|(rp, entry)| {
            let checkout = member_checkout_dir(rp, primary_root, workweave_dir);
            let vcs = vcs_for(entry.vcs_type);
            let old = vcs.head_revision(checkout.path()).ok();
            let (expected, declared) = aligner.wanted(entry.role, entry);
            WorkItem {
                repo_path: rp.clone(),
                entry: entry.clone(),
                checkout,
                old,
                vcs,
                planned: None,
                expected: LocalRefName::of_branch(&expected),
                feature: !declared,
            }
        })
        .collect();
    let work_items = plan_url_remotes(work_items)?;

    let parallel = jobs > 1;
    // Under JSON mode we suppress text-mode prefix output so stdout only
    // carries structured data. Text mode uses the Reporter prefix as usual.
    let use_reporter = output == OutputMode::Text;
    let write_lock: Mutex<()> = Mutex::new(());

    let reporter_for = |item: &WorkItem| {
        if parallel && use_reporter {
            Reporter::parallel(item.repo_path.as_str().to_string(), &write_lock)
        } else {
            Reporter::serial()
        }
    };
    let fetched: Vec<Fetched> = match &state.resumed {
        Some((_, record)) => work_items
            .iter()
            .map(|item| {
                match record
                    .upstream_tips
                    .get(&RepoKey::Member(item.repo_path.clone()))
                {
                    Some(tip) => Ok(Some(ResolvedRevisionId::from(tip.clone()))),
                    None if item.feature => Ok(None),
                    None => Err(format!(
                        "{}: the op recorded no upstream tip",
                        item.repo_path
                    )),
                }
            })
            .collect(),
        None => run_in_parallel(&work_items, jobs, |_idx, item| {
            fetch_and_resolve(item, &reporter_for(item), use_reporter)
        }),
    };
    refuse_backwards(
        &work_items,
        &fetched,
        state,
        &project,
        &crate::alignment::Aligner::open(
            active_root,
            project_name,
            crate::alignment::Accounting::scan(primary_root)?,
        )?,
        workweave_dir.is_some(),
    )?;
    let settled = settle_diverged(ctx, project_name, &work_items, &fetched, selection, state)?;

    let outcomes: Vec<RepoOutcome> = run_in_parallel(&work_items, jobs, |idx, item| {
        if settled.as_refs.contains(&idx) || selection.branch_only(&item.repo_path, item.entry.role)
        {
            let head = item
                .vcs
                .head_revision(item.checkout.path())
                .map(|head| head.display_str().to_owned())
                .map_err(|e| format!("{}: {e:#}", item.repo_path))?;
            return Ok((head, None));
        }
        let resolved = match &fetched[idx] {
            Ok(Some(resolved)) => resolved,
            Ok(None) => {
                return item
                    .vcs
                    .head_revision(item.checkout.path())
                    .map(|head| (head.display_str().to_owned(), None))
                    .map_err(|e| format!("{}: {e:#}", item.repo_path));
            }
            Err(message) => return Err(message.clone()),
        };
        advance_one(item, resolved).and_then(|new_sha| {
            crate::settle::settle_moved(
                item.vcs.as_ref(),
                item.checkout.path(),
                item.entry.role,
                item.old.as_ref(),
            )
            .map(|settled| (new_sha, settled.files))
            .map_err(|e| format!("{}: {e:#}", item.repo_path))
        })
    });

    // Aggregate errors in input order — matches the existing serial shape.
    let mut errors: Vec<String> = Vec::new();
    let mut updated = 0usize;

    // Build JSON records if requested. We zip work_items with outcomes by
    // position (run_in_parallel preserves input order in its output).
    let mut json_records: Vec<RepoUpdateRecord> = Vec::new();
    let mut placements: Vec<PlacementOutput> = Vec::new();

    for (item, outcome) in work_items.iter().zip(outcomes) {
        let branch = item.entry.branch.to_string();
        let abs_str = crate::path_spelling::WirePath::of(item.checkout.path());

        let old_sha = item.old.as_ref().map(|r| r.display_str().to_owned());
        let mut placed = None;
        let record = match outcome {
            Ok((new_sha, files)) => {
                placed = files;
                // "advanced N repo(s)" counts SHA deltas, not non-`Err`
                // outcomes: a repo that was already at the target was
                // visited successfully but advanced nothing.
                let kind = if old_sha.as_deref() == Some(new_sha.as_str()) {
                    UpdateKind::UpToDate
                } else {
                    updated += 1;
                    UpdateKind::Updated
                };
                RepoUpdateRecord {
                    path: item.repo_path.to_string(),
                    absolute_path: abs_str,
                    branch,
                    kind,
                    old_sha,
                    new_sha: Some(new_sha),
                    error: None,
                }
            }
            Err(msg) => {
                errors.push(msg.clone());
                RepoUpdateRecord {
                    path: item.repo_path.to_string(),
                    absolute_path: abs_str,
                    branch,
                    kind: UpdateKind::Failed,
                    old_sha,
                    new_sha: None,
                    error: Some(msg),
                }
            }
        };

        if let (OutputMode::Text, Some(files)) = (output, placed) {
            outln!("  {}: files {files}", record.path);
        }
        let placement =
            placed.map(|files| PlacementOutput::new(&item.repo_path, item.checkout.path(), files));

        if output.is_json() {
            json_records.push(record);
            placements.extend(placement);
        }
    }

    if !errors.is_empty() && output == OutputMode::Text {
        eprintln!(
            "rwv update --remote: {} repo(s) failed to update:",
            errors.len()
        );
        for msg in &errors {
            eprintln!("  - {msg}");
        }
        crate::refuse!(
            crate::refusal::RefusalKind::PartialRunAborted,
            "update aborted with {} failure(s); lock not written",
            errors.len()
        );
    }

    if output == OutputMode::Text {
        outln!("rwv update --remote: advanced {updated} repo(s)");
    }

    // Under JSON mode with failures: emit output first, then bail.
    // Under text mode with failures: already bailed above.
    // Under JSON mode without failures: fall through to lock write.
    if output.is_json() && !errors.is_empty() {
        let envelope = UpdateRemoteJsonOutput {
            schema_url: UPDATE_REMOTE_SCHEMA_URL.to_string(),
            repos: json_records,
            placements,
            resolution: Some(ctx.resolution()),
            skipped: Vec::new(),
        };
        let out =
            serde_json::to_string_pretty(&envelope).context("failed to serialize update output")?;
        outln!("{out}");
        crate::refuse!(
            crate::refusal::RefusalKind::PartialRunAborted,
            "update aborted with {} failure(s); lock not written",
            errors.len()
        );
    }

    // Record the advanced members' tips. This happens after the worker pool
    // has joined: the lock is shared project-wide state, and concurrent writes
    // would race. The `--role` / `--repo` filter selects the members recorded
    // as well as the ones advanced; every other pin stays as it was.
    if let Some(op) = &settled.op {
        crate::op_state::set_phase(&op.workspace, crate::op_state::OpPhase::Relock)?;
    }
    let pending_commit = lock::relock(
        ctx,
        &lock::Relocking {
            verb: "update --remote",
            acting: filter,
            dirty,
            first_pins: lock::FirstPins::Taken,
            unaligned: lock::Unaligned::Placed,
            whole: lock::WholeFile::default(),
            op: if settled.op.is_some() {
                lock::OpHeld::ByThisRun
            } else {
                lock::OpHeld::None
            },
            by_name: Some(&settled.named),
        },
        commit,
    )
    .context("failed to write lock after update")?
    .map(|pending| pending.by_name("update --remote", settled.named.iter().cloned()));

    // Regeneration reads EVERY manifest member (member presence is gated on
    // `.exists()`, and content-derived fields read member working trees), so it
    // is only sound over a tree the run actually covered. An unfiltered update
    // bails above if any member is missing or failed to advance; a filtered one
    // proves nothing about the repos it skipped, so it leaves the managed files
    // alone and `rwv doctor --fix` remains the repair path.
    let mut authored = BTreeSet::new();
    let skipped = if filter.is_empty() {
        let skipped = match workweave_dir {
            Some(dir) => crate::activate::activate_workweave_intent(project_name, dir),
            None => crate::activate::activate_intent_with_options(project_name, ctx, opts),
        }
        .context("failed to regenerate integration content after update")?;
        authored = crate::activate::owned_paths(active_root, project_name, &project.manifest);

        // Report at the moment of causation. Advancing members can raise what
        // they require above a go.work go-line (or any other
        // `Ownership::DefaultOnly` value) the operator pinned — the MOVE is
        // valid and stays, but this is where the operator is standing when the
        // breach appears, so the observation is made here as well as in
        // `rwv doctor`. Same predicate, via the same runner: no policy can
        // diverge between the two surfacings. Not a refusal and not a gate —
        // the findings go to stderr so `--json` stdout stays structured, and
        // the exit status is untouched.
        //
        // Sharing the unfiltered branch deliberately: a filtered update leaves
        // the managed files alone and proves nothing about the members it
        // skipped, so it has no post-MOVE state worth reporting on either.
        for issue in
            crate::activate::member_incompatibilities(active_root, project_name, &project.manifest)
        {
            eprintln!("[warning] {}: {}", issue.source, issue.message);
        }
        skipped
    } else {
        let skipped = crate::skipped::SkippedEffect::narrowed().to_vec();
        skipped
            .iter()
            .for_each(crate::skipped::SkippedEffect::report);
        skipped
    };

    // The regenerated content and the lock that records the tips it was
    // derived from are one change: commit them together, after authoring.
    // When authoring was withheld, `authored` is empty and this is the
    // lock-only commit `rwv lock --commit` makes.
    if let Some(pending) = &pending_commit {
        lock::commit_project_lock(pending, &authored)
            .context("failed to commit lock after update")?;
    }

    if let Some(op) = &settled.op {
        op.finish(&settled.repos);
    }

    // Emit JSON envelope after lock write (so the lock is coherent before
    // consumers read the envelope).
    if output.is_json() {
        let envelope = UpdateRemoteJsonOutput {
            schema_url: UPDATE_REMOTE_SCHEMA_URL.to_string(),
            repos: json_records,
            placements,
            resolution: Some(ctx.resolution()),
            skipped,
        };
        let out =
            serde_json::to_string_pretty(&envelope).context("failed to serialize update output")?;
        outln!("{out}");
    }

    Ok(())
}

/// Find each member's url remote before anything is fetched or added: a name
/// held by another url refuses for every member that has one.
fn plan_url_remotes(mut items: Vec<WorkItem>) -> anyhow::Result<Vec<WorkItem>> {
    let mut report = crate::diagnosis::Report::default();
    for item in &mut items {
        if !item.checkout.path().exists() {
            continue;
        }
        let Ok(info) = crate::registry::resolve_to_clone_info(&item.entry.url) else {
            continue;
        };
        match crate::url_remote::find(
            item.vcs.as_ref(),
            &item.repo_path,
            "update",
            item.checkout.path(),
            &info.url,
        )?
        .plan()
        {
            Ok(planned) => item.planned = Some(planned),
            Err(judged) => report.add(*judged),
        }
    }
    report.into_result()?;
    Ok(items)
}

/// Per-repo worker: the pruning fetch of every remote, the url remote added
/// first, then the tip of the branch on the member's url remote.
///
/// `use_reporter` suppresses progress lines under `--json` so stdout carries
/// only structured data.
///
/// All user-facing text output is routed through `reporter`, which prefixes
/// `[<repo>]` and serializes writes under `-j > 1`; under `-j 1` the
/// reporter is a no-prefix passthrough.
fn fetch_and_resolve(item: &WorkItem, reporter: &Reporter<'_>, use_reporter: bool) -> Fetched {
    let vcs = item.vcs.as_ref();
    let repo_path = &item.repo_path;
    let entry = &item.entry;
    let planned = item.planned.as_ref();
    let repo_dir = item.checkout.path();

    if !repo_dir.exists() {
        // `rwv update` re-clones missing members of the active
        // project — the settled repair verb for dangling references. See
        // update::run_update.
        return Err(format!(
            "{}: clone missing on disk; run `rwv update` from the workspace \
             to re-materialize missing manifest members, then re-run `rwv update --remote`",
            repo_path.as_str(),
        ));
    }

    let branch = entry.branch.on_remote();
    let Some(planned) = planned else {
        return Err(format!(
            "{}: its url '{}' resolves to no remote, so there is no remote to advance \
             from; respell the entry's `url` in rwv.toml",
            repo_path.as_str(),
            entry.url,
        ));
    };

    // The pruning fetch of every remote, the url remote added first.
    //
    // `--prune` removes remote-tracking refs that no longer exist on the
    // remote. Without it, a branch deleted or renamed upstream leaves a
    // stale `origin/<branch>` ref that `resolve_branch_on_remote` would
    // resolve forever, silently diverging from the real upstream state
    // (R18 GAP). Pruning is safe: remote-tracking refs are derived state;
    // the lock pins bare SHAs, so pruned refs never lose reachable objects.
    //
    // Shallow-clone note: `git fetch --prune` works correctly for shallow
    // clones (per git-fetch(1)); git does not refuse or warn on shallow
    // repos for the `--prune` flag. No special handling needed.
    if use_reporter {
        reporter.out(&format!(
            "rwv update --remote: fetching {}",
            repo_path.as_str()
        ));
    }
    let fetched = crate::fetch::pruning_fetch(vcs, repo_dir, planned.add.as_ref())
        .map_err(|e| format!("{}: fetch failed: {e:#}", repo_path.as_str()))?;
    if let Some(remote) = fetched.unconfirmed.first() {
        return Err(format!(
            "{}: fetch failed: remote {}: {}",
            repo_path.as_str(),
            remote.remote,
            remote.cause
        ));
    }
    if use_reporter {
        for line in fetched.log.lines() {
            reporter.err(line);
        }
    }

    if item.feature {
        return feature_upstream_tip(item).map_err(|e| format!("{}: {e:#}", repo_path.as_str()));
    }

    // Resolve the branch HEAD on the conventional remote. The VCS layer
    // owns the naming policy, so this is one call rather than a fallback
    // chain. No bare-branch fallback — missing-remote produces a clear
    // error rather than silently resolving to the local branch tip.
    //
    // When `--prune` above removed a stale remote-tracking ref for
    // `branch`, this resolution will now correctly fail rather than
    // resolving against a ghost ref. The error below names the state and
    // the two actionable exits so the operator knows what to do.
    vcs.resolve_branch_on_remote(repo_dir, &planned.remote, &branch)
        .map(Some)
        .map_err(|_| {
            format!(
                "{repo}: branch '{branch}' does not resolve on the remote \
                 — renamed or deleted upstream, or `branch` was never a \
                 branch (it tracks a branch only; it cannot pin a tag or \
                 SHA). To fix: update rwv.toml's `branch` field to the \
                 current branch name.",
                repo = repo_path.as_str(),
                branch = branch.branch(),
            )
        })
}

/// The tip of the feature branch's own upstream as last fetched: the remote
/// branch it takes in from, or the same-named branch it is given first.
/// `None` where it has none, or takes in from a branch of another name, which
/// [`refuse_backwards`] refuses.
fn feature_upstream_tip(item: &WorkItem) -> anyhow::Result<Option<ResolvedRevisionId>> {
    let vcs = item.vcs.as_ref();
    let dir = item.checkout.path();
    let branch = RawRefName::new(item.expected.as_str());
    let remote = match crate::alignment::find_e(vcs, dir, &item.entry, &branch)? {
        crate::alignment::Found::OnRemote { tip, .. } => return Ok(Some(tip)),
        crate::alignment::Found::LocalGivenUpstream { upstream, .. } => upstream.remote,
        crate::alignment::Found::Local { .. } => match vcs.upstream_of(dir, &branch)? {
            Some(upstream) => upstream.remote,
            None => return Ok(None),
        },
        crate::alignment::Found::Foreign { .. } | crate::alignment::Found::Nowhere => {
            return Ok(None)
        }
    };
    Ok(vcs
        .remote_copies(dir, &branch)?
        .into_iter()
        .find(|copy| copy.remote == remote)
        .map(|copy| copy.tip))
}

/// Refuse, before any member moves, every member whose declared branch exists
/// nowhere, every feature branch that takes in from a branch of another name,
/// and every member whose re-pin does not descend from the commit the lock
/// pins for it: recording it would move the pin backward, and only a verb that
/// names the member may do that. The re-pin is the branch's own tip where the
/// branch already holds the upstream's or merges it, else the upstream's tip;
/// a member `--reset-to-upstream` names is re-pinned by name.
fn refuse_backwards(
    items: &[WorkItem],
    fetched: &[Fetched],
    state: &State,
    project: &Project,
    aligner: &crate::alignment::Aligner,
    in_workweave: bool,
) -> anyhow::Result<()> {
    let lock = project.lock.as_ref();
    let mut report = crate::diagnosis::Report::default();
    for (item, tip) in items.iter().zip(fetched) {
        let dir = item.checkout.path();
        let vcs = item.vcs.as_ref();
        if dir.exists() {
            let branch = RawRefName::new(item.expected.as_str());
            match crate::alignment::find_e(vcs, dir, &item.entry, &branch)? {
                crate::alignment::Found::Nowhere if !item.feature => {
                    report.add(
                        crate::alignment::declared_nowhere(&item.repo_path, dir, &branch)
                            .by_verb("update")
                            .judge(vcs),
                    );
                    continue;
                }
                crate::alignment::Found::Foreign { upstream } if item.feature => {
                    report.add(
                        crate::diagnosis::State::PushBranchForeignUpstream
                            .of(&item.repo_path)
                            .member()
                            .bind("<m>", item.repo_path.as_str())
                            .bind("<repo>", item.repo_path.as_str())
                            .subject(format!(
                                "{branch} takes in from {}/{}",
                                upstream.remote, upstream.branch
                            ))
                            .checkout(dir)
                            .judge(vcs),
                    );
                    continue;
                }
                _ => {}
            }
        }
        let Ok(Some(tip)) = tip else { continue };
        if state.flags.resets(&crate::manifest::RepoOperand::Member(
            item.repo_path.clone(),
        )) {
            continue;
        }
        let (label, repin) = match site_of(item) {
            Some((_, local)) => match vcs.relation(dir, &local, tip) {
                Ok(Relation::Ahead) => ("the branch's tip", local),
                Ok(Relation::Diverged) if state.flags.merge_upstream => ("the branch's tip", local),
                _ => ("the upstream's tip", tip.clone()),
            },
            None => ("the upstream's tip", tip.clone()),
        };
        let subject = match crate::alignment::pin_of(vcs, dir, lock, &item.repo_path)? {
            crate::alignment::Pin::None => continue,
            crate::alignment::Pin::Unreachable => "the pin is in no ref here".to_owned(),
            crate::alignment::Pin::At(pin) => match vcs.relation(dir, &repin, &pin) {
                Ok(Relation::Even | Relation::Ahead) | Err(_) => continue,
                Ok(relation) => lock::backwards_subject(vcs, dir, label, &repin, &pin, relation),
            },
        };
        let member = crate::alignment::Member {
            path: &item.repo_path,
            entry: &item.entry,
            dir,
            vcs,
        };
        let candidate = lock::with_pins(&project.manifest, lock, [(&item.repo_path, &repin)]);
        let aligns = !in_workweave || item.checkout.is_workweave_slot();
        let rev = repin.id().short().to_owned();
        let offered = crate::member_pin::pin_refusals(aligner, &member, &candidate, aligns)?
            .is_empty()
            .then_some(rev.as_str());
        report.add(lock::backwards_line(
            &item.repo_path,
            dir,
            vcs,
            subject,
            offered,
        ));
    }
    report.into_result()
}

/// Advance the checkout onto `resolved`, the tip fetched for it
/// ([`advance_checkout`]), and return the commit it is then at.
fn advance_one(item: &WorkItem, resolved: &ResolvedRevisionId) -> Result<String, String> {
    let vcs = item.vcs.as_ref();
    let repo_dir = item.checkout.path();
    advance_checkout(item, resolved)?;

    // Capture the new SHA after checkout for JSON reporting.
    let new_sha = vcs
        .head_revision(repo_dir)
        .ok()
        .map(|r| r.display_str().to_owned())
        .unwrap_or_else(|| resolved.to_string());

    Ok(new_sha)
}

/// Advance one checkout onto `target`, the tip fetched for its branch.
///
/// A checkout attached to the branch it is expected on is fast-forwarded;
/// one that is not fast-forwardable refuses, naming the reconcile. A checkout
/// attached to anything else refuses, naming `rwv update`. Inside a workweave
/// a member that follows its declared branch advances the ref the checkout is
/// on when `target` is a fast-forward, and `rwv sync` is named otherwise. A
/// detached HEAD moves, under the mid-operation precondition of
/// [`Vcs::advance_detached_head`]. An unborn branch refuses.
fn advance_checkout(item: &WorkItem, target: &ResolvedRevisionId) -> Result<(), String> {
    let vcs = item.vcs.as_ref();
    let repo_path = &item.repo_path;
    let entry = &item.entry;
    let repo_dir = item.checkout.path();
    let in_workweave = item.checkout.is_workweave_slot() && !item.feature;
    let describe = |e: VcsError| format!("{}: {e:#}", repo_path.as_str());

    let attached = match vcs.head_attachment(repo_dir).map_err(describe)? {
        HeadAttachment::Detached(was) => {
            if was.at() == target {
                return Ok(());
            }
            return vcs.advance_detached_head(&was, target).map_err(describe);
        }
        HeadAttachment::Unborn(u) => {
            return Err(format!(
                "{}: branch '{u}' has no commits yet — rwv update --remote has no way \
                 to advance it to {}. Make an initial commit, or check out a branch \
                 that has one, then re-run.",
                repo_path.as_str(),
                target.display_str(),
            ));
        }
        HeadAttachment::Attached(a) => a,
    };

    let head = vcs.head_revision(repo_dir).map_err(describe)?;
    let holds_target = vcs.is_ancestor(repo_dir, target, &head).map_err(describe)?;
    let is_fast_forward =
        &head == target || vcs.is_ancestor(repo_dir, &head, target).map_err(describe)?;

    if in_workweave {
        if holds_target {
            return Ok(());
        }
        if is_fast_forward {
            return vcs
                .advance_attached_ref(&attached, target)
                .map_err(describe);
        }
        return Err(format!(
            "{}: advancing '{attached}' to {} is not a fast-forward — this \
             workweave's checkout has diverged from that tip.\n  \
             Run `rwv sync` to reconcile the workweave with its parent, then \
             re-run `rwv update --remote`.",
            repo_path.as_str(),
            target.display_str(),
        ));
    }

    let counterpart = &item.expected;

    if item.feature && !attached.is_named(counterpart) {
        return Err(format!(
            "{}: is on branch '{attached}', not '{counterpart}', the branch it sits on \
             in this feature — rwv update --remote moves only that branch.\n  \
             Run `rwv update` to align the member, then re-run.",
            repo_path.as_str(),
        ));
    }
    if !attached.is_named(counterpart) {
        return Err(format!(
            "{}: is on branch '{attached}', not the local counterpart \
             ('{counterpart}') of the branch the manifest declares — \
             rwv update --remote moves only that branch, so it will not relocate one it \
             cannot relate to the tracking declaration.\n  \
             Run `rwv update` to align the member, then re-run.",
            repo_path.as_str(),
        ));
    }

    if holds_target {
        return Ok(());
    }
    if is_fast_forward {
        return vcs
            .advance_attached_ref(&attached, target)
            .map_err(describe);
    }
    Err(format!(
        "{}: advancing '{attached}' to {} is not a fast-forward — the branch \
         carries commits '{declared}' on the remote does not have.\n  \
         Reconcile '{attached}' with its tracking tip yourself (ordinary \
         `git rebase` / `git merge`) and re-run.",
        repo_path.as_str(),
        target.display_str(),
        declared = entry.branch,
    ))
}

/// What the diverged members came to.
struct Settled {
    op: Option<Op>,
    repos: Vec<(PathBuf, Box<dyn Vcs>)>,
    /// Members whose branch moved as a ref: their checkouts stay where they are.
    as_refs: BTreeSet<usize>,
    /// Members `--reset-to-upstream` names.
    named: BTreeSet<RepoPath>,
}

enum Site {
    Checkout(AttachedRef),
    Refs(RawRefName),
}

struct Divergence {
    idx: usize,
    site: Site,
    local: ResolvedRevisionId,
    relation: Relation,
}

/// Where the branch a member's advance moves lives, with its tip: attached
/// in the checkout, or checked out nowhere.
fn site_of(item: &WorkItem) -> Option<(Site, ResolvedRevisionId)> {
    let dir = item.checkout.path();
    if !dir.exists() {
        return None;
    }
    let vcs = item.vcs.as_ref();
    let any_attached = item.checkout.is_workweave_slot() && !item.feature;
    match vcs.head_attachment(dir) {
        Ok(HeadAttachment::Attached(attached))
            if any_attached || attached.is_named(&item.expected) =>
        {
            vcs.head_revision(dir)
                .ok()
                .map(|head| (Site::Checkout(attached), head))
        }
        _ => {
            let branch = RawRefName::new(item.expected.as_str());
            match (
                vcs.resolve_local_branch_tip(dir, &branch),
                vcs.checkouts_of_branch(dir, &branch),
            ) {
                (Ok(Some(local)), Ok(held)) if held.is_empty() => Some((Site::Refs(branch), local)),
                _ => None,
            }
        }
    }
}

/// The members whose branch differs from its upstream tip, with the place the
/// branch lives.
fn divergences(items: &[WorkItem], fetched: &[Fetched]) -> Vec<Divergence> {
    let mut found = Vec::new();
    for (idx, (item, tip)) in items.iter().zip(fetched).enumerate() {
        let Ok(Some(tip)) = tip else { continue };
        let Some((site, local)) = site_of(item) else {
            continue;
        };
        let Ok(relation) = item.vcs.relation(item.checkout.path(), &local, tip) else {
            continue;
        };
        if relation != Relation::Even {
            found.push(Divergence {
                idx,
                site,
                local,
                relation,
            });
        }
    }
    found
}

/// A member branch other than the one this workspace expects that a commit
/// of the project branch's unpushed range declares, diverged from its
/// upstream and checked out nowhere: what `push` publishes and only a merge
/// or reset as refs can settle.
struct DeclaredElsewhere {
    idx: usize,
    branch: RawRefName,
    local: ResolvedRevisionId,
    upstream: ResolvedRevisionId,
    relation: Relation,
    remote: Option<crate::vcs::RemoteHandle>,
}

/// The branches [`DeclaredElsewhere`] names, and each commit of the unpushed
/// project range with the lock it records.
#[derive(Default)]
struct Elsewhere {
    found: Vec<DeclaredElsewhere>,
    range_locks: Vec<(crate::vcs::CommitId, crate::manifest::LockFile)>,
}

/// Each branch `named` names, judged as `<m>:<branch>` must be: a branch of
/// a member this update acts on, not the one this workspace expects for it,
/// that a commit of the project branch's unpushed range declares, that
/// exists here and no checkout holds; the ones off their upstream found.
fn declared_elsewhere(
    items: &[WorkItem],
    project_dir: &Path,
    named: &[BranchOperand],
) -> anyhow::Result<Elsewhere> {
    use crate::refusal::RefusalKind::InapplicableFlag;
    if named.is_empty() {
        return Ok(Elsewhere::default());
    }
    let pvcs = crate::vcs::project_vcs();
    let HeadAttachment::Attached(attached) = pvcs.head_attachment(project_dir)? else {
        crate::refuse!(
            InapplicableFlag,
            "`{}` names a branch a commit of the project branch's unpushed range declares, and \
             the project repo is on no branch",
            named[0]
        );
    };
    let raw = RawRefName::new(attached.to_string());
    let tip = pvcs.head_revision(project_dir)?;
    let url_remote = pvcs.project_url_remote(project_dir)?;
    let declared = crate::push::declared_by_unpushed_range(
        pvcs.as_ref(),
        project_dir,
        &raw,
        &tip,
        url_remote.as_ref(),
    )?;
    let mut found = Vec::new();
    for operand in named {
        let Some((idx, item)) = items
            .iter()
            .enumerate()
            .find(|(_, item)| item.repo_path == operand.repo)
        else {
            crate::refuse!(
                InapplicableFlag,
                "`{operand}` names no member this update acts on"
            );
        };
        let dir = item.checkout.path();
        let vcs = item.vcs.as_ref();
        let branch = &operand.branch;
        if branch.as_str() == item.expected.as_str() {
            crate::refuse!(
                InapplicableFlag,
                "`{operand}` names '{branch}', the branch this workspace expects for {m}; name \
                 it `{m}`",
                m = item.repo_path
            );
        }
        let declares = !item.feature
            && matches!(
                item.entry.role,
                crate::manifest::Role::Owned | crate::manifest::Role::Fork
            )
            && declared
                .get(&item.repo_path)
                .is_some_and(|names| names.contains(branch.as_str()));
        if !declares {
            crate::refuse!(
                InapplicableFlag,
                "`{operand}`: no commit of '{raw}' its push branch lacks declares '{branch}' for \
                 {m}; `<m>:<branch>` names only such a branch, and `{m}` names the one this \
                 workspace expects",
                m = item.repo_path
            );
        }
        let Some(local) = vcs.resolve_local_branch_tip(dir, branch)? else {
            crate::refuse!(
                InapplicableFlag,
                "`{operand}`: {m} has no branch '{branch}' here",
                m = item.repo_path
            );
        };
        if let Some(holder) = vcs.checkouts_of_branch(dir, branch)?.into_iter().next() {
            crate::refuse!(
                InapplicableFlag,
                "`{operand}`: '{branch}' is checked out in {}; run `rwv update --remote` in the \
                 workspace holding it, naming `{m}`",
                holder.display(),
                m = item.repo_path
            );
        }
        let Some((upstream_ref, upstream)) =
            crate::push_branch::upstream_copy(vcs, dir, branch, None)?
        else {
            crate::refuse!(
                InapplicableFlag,
                "`{operand}`: '{branch}' takes in from no upstream",
            );
        };
        let relation = vcs.relation(dir, &local, &upstream)?;
        if relation == Relation::Even {
            continue;
        }
        let remote = match upstream_ref {
            crate::branch_ref::BranchRef::Remote { remote, .. } => Some(remote),
            crate::branch_ref::BranchRef::Local(_) => None,
        };
        found.push(DeclaredElsewhere {
            idx,
            branch: branch.clone(),
            local,
            upstream,
            relation,
            remote,
        });
    }
    let range_locks = match found.is_empty() {
        true => Vec::new(),
        false => crate::push::locks_of_unpushed_range(
            pvcs.as_ref(),
            project_dir,
            &raw,
            &tip,
            url_remote.as_ref(),
        )?,
    };
    Ok(Elsewhere { found, range_locks })
}

/// `e`'s merge as refs conflicts in `paths`, and no workspace can be opened
/// with it checked out: the routes that settle it without a merge. Where the
/// tip's declared branch holds every unpushed commit of `e`'s branch, the
/// reset alone; else a recorded copy onto it, or publishing the branch under
/// another name, then the reset. Each project commit of the unpushed range
/// whose pin a reset leaves only a backup holding is listed.
fn judge_conflicting_as_refs(
    item: &WorkItem,
    e: &DeclaredElsewhere,
    paths: &[String],
    range_locks: &[(crate::vcs::CommitId, crate::manifest::LockFile)],
    project: &ProjectName,
) -> anyhow::Result<crate::diagnosis::Judged> {
    use crate::diagnosis::Form;
    let vcs = item.vcs.as_ref();
    let dir = item.checkout.path();
    let declared = RawRefName::new(item.expected.as_str());
    let unpushed = vcs.unpushed_commits(dir, &e.branch)?;
    let declared_tip = vcs.resolve_local_branch_tip(dir, &declared)?;
    let mut off_declared = Vec::new();
    for commit in &unpushed {
        let at = ResolvedRevisionId::from(commit.id.clone());
        let held = match &declared_tip {
            Some(tip) => vcs.is_ancestor(dir, &at, tip)?,
            None => false,
        };
        if !held {
            off_declared.push(commit.clone());
        }
    }
    let copied = match &declared_tip {
        Some(tip) => {
            let ids: Vec<crate::vcs::CommitId> = off_declared
                .iter()
                .map(|commit| commit.id.clone())
                .collect();
            vcs.copied_into(dir, &ids, tip)?
        }
        None => BTreeSet::new(),
    };
    let lacking: Vec<&crate::vcs::CommitSummary> = off_declared
        .iter()
        .filter(|commit| !copied.contains(&commit.id))
        .collect();
    let (ahead, behind) = vcs.ahead_behind(dir, e.upstream.id(), e.local.id());
    let mut listing: Vec<String> = Vec::new();
    for (project_commit, lock) in range_locks {
        let Some(pin) = lock
            .get_entry(&item.repo_path)
            .and_then(|entry| vcs.resolve_revision(dir, entry.version.as_str()).ok())
        else {
            continue;
        };
        if !off_declared.iter().any(|commit| &commit.id == pin.id()) {
            continue;
        }
        let route = match copied.contains(pin.id()) {
            true => format!("`{declared}` holds a recorded copy of it"),
            false => "copied and reset, the tip's pin carries a recorded copy of it".to_owned(),
        };
        listing.push(format!(
            "projects/{project} {} pins {}, which after the reset only a backup holds; {route}",
            project_commit.short(),
            pin.id().short()
        ));
    }
    let mut judging = crate::diagnosis::State::PushBranchDiverged
        .of(&item.repo_path)
        .member()
        .checkout(dir)
        .by_verb(crate::diagnosis::UPDATE_REMOTE)
        .bind("<m>", item.repo_path.as_str())
        .bind("<repo>", format!("{}:{}", item.repo_path, e.branch))
        .bind("<b>", &declared)
        .subject(format!(
            "{ahead} ahead, {behind} behind; '{}' is checked out nowhere and its merge as refs \
             conflicts in {}",
            e.branch,
            paths.join(", ")
        ));
    if lacking.is_empty() {
        listing.insert(
            0,
            format!(
                "`{declared}` holds each commit of '{}' its upstream lacks",
                e.branch
            ),
        );
        return Ok(judging
            .listing(listing)
            .only_forms(&[Form::C3HeldByDeclared])
            .judge(vcs));
    }
    let mut lines: Vec<String> = lacking
        .iter()
        .map(|commit| format!("{} {}", commit.id.short(), commit.subject))
        .collect();
    lines.extend(listing);
    judging = judging
        .listing(lines)
        .bind_list(
            "<ids>",
            lacking
                .iter()
                .rev()
                .map(|commit| commit.id.short().to_owned()),
        )
        .bind("<sha>", e.local.id().short());
    let mut forms = Vec::new();
    if let Some(onto) = vcs.checkouts_of_branch(dir, &declared)?.into_iter().next() {
        judging = judging.bind_place("<b-checkout>", &onto);
        forms.push(Form::C15ThenReset);
    }
    if let Some(remote) = &e.remote {
        judging = judging.bind("<r>", remote);
        forms.push(Form::C27ThenReset);
    }
    Ok(judging.only_forms(&forms).judge(vcs))
}

/// Refuse, all together and before anything moves, each member that has
/// diverged and no flag takes in; then reset and merge the ones a flag names,
/// under the op record.
fn settle_diverged(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    items: &[WorkItem],
    fetched: &[Fetched],
    selection: &Selection,
    state: &State,
) -> anyhow::Result<Settled> {
    let mut settled = Settled {
        op: None,
        repos: Vec::new(),
        as_refs: BTreeSet::new(),
        named: items
            .iter()
            .map(|item| &item.repo_path)
            .filter(|path| {
                state
                    .flags
                    .resets(&crate::manifest::RepoOperand::Member((*path).clone()))
            })
            .cloned()
            .collect(),
    };
    let mut found = divergences(items, fetched);
    found.retain(|d| {
        let item = &items[d.idx];
        !selection.branch_only(&item.repo_path, item.entry.role)
    });
    let project_vcs = crate::vcs::project_vcs();
    let project_dir = crate::workspace::project_dir(ctx.active_path(), project);
    for named in &state.flags.reset_to_upstream {
        let is_member = matches!(&named.repo, crate::manifest::RepoOperand::Member(path)
            if items.iter().any(|item| item.repo_path == *path));
        if !is_member {
            crate::refuse!(
                crate::refusal::RefusalKind::InapplicableFlag,
                "`--reset-to-upstream {named}` names no member this update acts on; the project \
                 repo is reset by `rwv pull --reset-to-upstream {}`",
                named.repo
            );
        }
    }
    let mut named_branches: Vec<BranchOperand> = selection.branches.clone();
    for named in &state.flags.reset_to_upstream {
        if let (crate::manifest::RepoOperand::Member(repo), Some(branch)) =
            (&named.repo, &named.branch)
        {
            let operand = BranchOperand {
                repo: repo.clone(),
                branch: RawRefName::new(branch.clone()),
            };
            if !named_branches.contains(&operand) {
                named_branches.push(operand);
            }
        }
    }
    let elsewhere = declared_elsewhere(items, &project_dir, &named_branches)?;
    let mut report = crate::diagnosis::Report::default();
    let mut acting: Vec<(&Divergence, bool)> = Vec::new();
    let lineage = if state.flags.merge_upstream && !found.is_empty() {
        crate::feature_sync::feature_lineage(ctx, project)?
    } else {
        None
    };
    let ours = crate::manifest::Manifest::from_path(
        &project_dir.join(crate::manifest::Manifest::FILE_NAME),
    )?;
    for d in &found {
        let item = &items[d.idx];
        let dir = item.checkout.path();
        let vcs = item.vcs.as_ref();
        let tip = fetched[d.idx]
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .expect("a divergence has a tip");
        let operand = crate::manifest::RepoOperand::Member(item.repo_path.clone());
        let reset = state.flags.resets(&operand);
        let merging = state.flags.merge_upstream && d.relation == Relation::Diverged;
        if !reset && !merging {
            if d.relation == Relation::Diverged {
                let (ahead, behind) = vcs.ahead_behind(dir, tip.id(), d.local.id());
                let mut judging = crate::diagnosis::State::PushBranchDiverged
                    .of(&item.repo_path)
                    .member()
                    .checkout(dir)
                    .bind("<m>", item.repo_path.as_str())
                    .bind("<repo>", item.repo_path.as_str())
                    .subject(format!("{ahead} ahead, {behind} behind"));
                if !upstream_is_push_branch(vcs, dir, d) {
                    judging = judging.only(&[crate::diagnosis::ChoiceId::C2]);
                }
                report.add(judging.judge(vcs));
            }
            continue;
        }
        if reset {
            let branch = match &d.site {
                Site::Checkout(attached) => RawRefName::new(attached.to_string()),
                Site::Refs(branch) => branch.clone(),
            };
            let tracked = match d.site {
                Site::Checkout(_) => vcs.tracked_dirty_file_names(dir).unwrap_or_default(),
                Site::Refs(_) => Vec::new(),
            };
            crate::merge_upstream::judge_reset(
                item.repo_path.as_str(),
                dir,
                vcs,
                Some(tip),
                &crate::merge_upstream::dependents_of(
                    project_vcs.as_ref(),
                    &project_dir,
                    branch.as_str(),
                )?,
                crate::diagnosis::judge_edits(vcs, &item.repo_path, dir, tracked, None),
                &mut report,
            );
        } else {
            if matches!(
                item.entry.role,
                crate::manifest::Role::Owned | crate::manifest::Role::Fork
            ) {
                if let Some(lineage) = &lineage {
                    report.extend(lineage.judge_merge_upstream(
                        &ours,
                        &item.repo_path,
                        dir,
                        vcs,
                        d.local.id(),
                        tip.id(),
                        crate::feature_sync::MergeUpstreamBy::UpdateRemote,
                    )?);
                }
            }
            match &d.site {
                Site::Checkout(_) => {
                    let tracked = vcs.tracked_dirty_file_names(dir).unwrap_or_default();
                    report.extend(crate::diagnosis::judge_edits(
                        vcs,
                        &item.repo_path,
                        dir,
                        tracked,
                        None,
                    ));
                }
                Site::Refs(_) => {
                    if let crate::vcs::RefMerge::Conflicted { paths } = vcs.merge_as_refs(
                        dir,
                        tip.id(),
                        d.local.id(),
                        "probe",
                        crate::vcs::DerivedContentPolicy::keep_incoming_side(),
                        tip.id(),
                    )? {
                        report.note(format!(
                            "{}: '{}' is checked out nowhere and its merge conflicts in {}; a \
                             conflict needs a workspace to stop in, so run it where the branch \
                             is checked out",
                            item.repo_path,
                            item.expected,
                            paths.join(", ")
                        ));
                    }
                }
            }
        }
        acting.push((d, reset));
    }
    let mut settling: Vec<(&DeclaredElsewhere, bool)> = Vec::new();
    for e in &elsewhere.found {
        let item = &items[e.idx];
        let dir = item.checkout.path();
        let vcs = item.vcs.as_ref();
        let reset = state.flags.resets_branch(
            &crate::manifest::RepoOperand::Member(item.repo_path.clone()),
            e.branch.as_str(),
        );
        if reset {
            crate::merge_upstream::judge_reset(
                item.repo_path.as_str(),
                dir,
                vcs,
                Some(&e.upstream),
                &[],
                None,
                &mut report,
            );
        } else if e.relation != Relation::Diverged {
            continue;
        } else if !state.flags.merge_upstream {
            let operand = BranchOperand {
                repo: item.repo_path.clone(),
                branch: e.branch.clone(),
            };
            let (ahead, behind) = vcs.ahead_behind(dir, e.upstream.id(), e.local.id());
            report.add(
                crate::diagnosis::State::PushBranchDiverged
                    .of(&item.repo_path)
                    .member()
                    .checkout(dir)
                    .bind("<m>", &operand)
                    .bind("<repo>", &operand)
                    .subject(format!("'{}' {ahead} ahead, {behind} behind", e.branch))
                    .only_forms(&[
                        crate::diagnosis::Form::C2UpdateRemote,
                        crate::diagnosis::Form::C3UpdateRemote,
                    ])
                    .judge(vcs),
            );
            continue;
        } else if let crate::vcs::RefMerge::Conflicted { paths } = vcs.merge_as_refs(
            dir,
            e.upstream.id(),
            e.local.id(),
            "probe",
            crate::vcs::DerivedContentPolicy::keep_incoming_side(),
            e.upstream.id(),
        )? {
            report.add(judge_conflicting_as_refs(
                item,
                e,
                &paths,
                &elsewhere.range_locks,
                project,
            )?);
        }
        settling.push((e, reset));
    }
    report.into_result()?;
    let moved: Vec<_> = acting
        .iter()
        .map(|(d, _)| {
            let item = &items[d.idx];
            (item.repo_path.clone(), item.expected.as_str().to_owned())
        })
        .chain(
            settling
                .iter()
                .map(|(e, _)| (items[e.idx].repo_path.clone(), e.branch.as_str().to_owned())),
        )
        .map(|(path, branch)| (crate::manifest::RepoKey::Member(path), branch))
        .collect();
    crate::landing_claim::refuse_moving_claimed(
        project_vcs.as_ref(),
        &project_dir,
        project,
        &moved,
        None,
    )?;
    if acting.is_empty() && settling.is_empty() && state.resumed.is_none() {
        return Ok(settled);
    }

    let mut repos: Vec<(PathBuf, Box<dyn Vcs>)> =
        vec![(project_dir.clone(), crate::vcs::project_vcs())];
    for item in items {
        if item.checkout.path().exists() {
            repos.push((
                item.checkout.path().to_path_buf(),
                vcs_for(item.entry.vcs_type),
            ));
        }
    }
    let op = match &state.resumed {
        Some((op, _)) => Op::adopt(op),
        None => {
            let mut record = state.flags.to_record();
            record.roles = state.wire.roles.clone();
            record.repos = state.wire.repos.clone();
            record.dirty = state.dirty;
            record.commit = state.committing.is_some();
            record.message = state.committing.clone().flatten();
            record.no_materialize = state.wire.no_materialize;
            for (item, tip) in items.iter().zip(fetched) {
                if let Ok(Some(tip)) = tip {
                    record
                        .upstream_tips
                        .insert(RepoKey::Member(item.repo_path.clone()), tip.id().clone());
                }
            }
            Op::begin(
                ctx.active_path(),
                project,
                OpVerb::UpdateRemote,
                record,
                &repos,
            )?
        }
    };
    let workspace = ctx.active_path();
    for (d, reset) in &acting {
        if !reset {
            continue;
        }
        let item = &items[d.idx];
        let tip = fetched[d.idx]
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .expect("a divergence has a tip");
        let (branch, on) = match &d.site {
            Site::Checkout(attached) => (RawRefName::new(attached.to_string()), Some(attached)),
            Site::Refs(branch) => (branch.clone(), None),
        };
        let backup = crate::merge_upstream::reset_to_upstream(
            workspace,
            RepoKey::Member(item.repo_path.clone()),
            item.vcs.as_ref(),
            item.checkout.path(),
            &branch,
            on,
            tip,
        )?;
        eprintln!(
            "rwv update --remote: {}: reset '{branch}' to its upstream {}, keeping {} in backup {backup}",
            item.repo_path,
            tip.id().short(),
            d.local.id().short()
        );
        if on.is_none() {
            settled.as_refs.insert(d.idx);
        }
    }
    for (d, reset) in &acting {
        if *reset {
            continue;
        }
        let item = &items[d.idx];
        let tip = fetched[d.idx]
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .expect("a divergence has a tip");
        let message = format!("Merge the upstream of {} into it", item.expected);
        match merge_member(
            matches!(d.site, Site::Refs(_)),
            workspace,
            &item.repo_path,
            &item.entry,
            item.checkout.path(),
            tip.id(),
            Order::SourceFirst,
            &message,
            crate::vcs::DerivedContentPolicy::keep_incoming_side(),
        ) {
            Ok(joined) => {
                if joined.tip().is_some() {
                    eprintln!(
                        "rwv update --remote: {}: merged its upstream {}",
                        item.repo_path,
                        tip.id().short()
                    );
                }
                if matches!(d.site, Site::Refs(_)) {
                    settled.as_refs.insert(d.idx);
                }
            }
            Err(MemberStop::Conflict(conflicted)) => {
                return Err(conflict_stop(
                    "update --remote",
                    &crate::op_state::resume_command(OpVerb::UpdateRemote),
                    &conflicted,
                    item.vcs.as_ref(),
                    &[],
                ));
            }
            Err(MemberStop::Refused(error)) => return Err(error),
        }
    }
    for (e, reset) in &settling {
        let item = &items[e.idx];
        let dir = item.checkout.path();
        if *reset {
            let backup = crate::merge_upstream::reset_to_upstream(
                workspace,
                RepoKey::Member(item.repo_path.clone()),
                item.vcs.as_ref(),
                dir,
                &e.branch,
                None,
                &e.upstream,
            )?;
            eprintln!(
                "rwv update --remote: {}: reset '{}' to its upstream {}, keeping {} in backup {backup}",
                item.repo_path,
                e.branch,
                e.upstream.id().short(),
                e.local.id().short()
            );
            continue;
        }
        let message = format!("Merge the upstream of {} into it", e.branch);
        match crate::project_merge::merge_branch_as_refs(
            workspace,
            &item.repo_path,
            item.vcs.as_ref(),
            dir,
            &e.branch,
            e.upstream.id(),
            Order::SourceFirst,
            &message,
            crate::vcs::DerivedContentPolicy::keep_incoming_side(),
        ) {
            Ok(_) => eprintln!(
                "rwv update --remote: {}: merged the upstream {} into '{}'",
                item.repo_path,
                e.upstream.id().short(),
                e.branch
            ),
            Err(MemberStop::Conflict(conflicted)) => {
                return Err(conflict_stop(
                    "update --remote",
                    &crate::op_state::resume_command(OpVerb::UpdateRemote),
                    &conflicted,
                    item.vcs.as_ref(),
                    &[],
                ));
            }
            Err(MemberStop::Refused(error)) => return Err(error),
        }
    }
    settled.op = Some(op);
    settled.repos = repos;
    Ok(settled)
}

fn upstream_is_push_branch(vcs: &dyn Vcs, dir: &Path, d: &Divergence) -> bool {
    let branch = match &d.site {
        Site::Checkout(attached) => RawRefName::new(attached.to_string()),
        Site::Refs(branch) => branch.clone(),
    };
    crate::push_branch::upstream_is_push_branch(vcs, dir, &branch, None)
}
