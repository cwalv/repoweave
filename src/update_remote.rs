//! `rwv update --remote` — advance each manifest repo to its branch HEAD and
//! re-snapshot `rwv.lock`.
//!
//! Semantically analogous to `cargo update` or `npm update`: this is the
//! form that mutates the lock by pulling fresh tips from the network.
//! `rwv update` aligns clones to the lock; `rwv lock` snapshots local tips;
//! `rwv update --remote` advances and re-snapshots.

use crate::lock;
use crate::manifest::{Project, ProjectName, RepoEntry, RepoPath};
use crate::parallel::{run_in_parallel, OutputMode, Reporter};
use crate::placement::PlacementOutput;
use crate::selector::RepoFilter;
use crate::vcs::{vcs_for, HeadAttachment, ResolvedRevisionId, Vcs, VcsError};
use crate::workspace::{
    member_checkout_dir, Checkout, MemberCheckout, Resolution, WorkspaceContext,
};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
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
/// After all repos are advanced, regenerate `rwv.lock` from the new tips
/// and write it. The lock-write reuses `lock::generate_lock`, which carries
/// the dirty check; `dirty` here controls whether to bypass it.
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
    filter: &RepoFilter,
    jobs: usize,
    opts: crate::activate::ActivateOptions,
) -> anyhow::Result<()> {
    // Only reads the op-state; the sync engine takes its own exclusion.
    crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;

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
        filter,
        jobs,
        opts,
    )
}

/// Outcome of advancing a single repo.
///
/// `Ok((new_sha, files))` means fetch + resolve + checkout ran cleanly, the
/// repo is now at `new_sha`, and `files` says how the files it rewrote were
/// placed. `Err(msg)` means one of those steps failed and `msg` is the
/// human-readable failure to surface in the aggregated summary.
type RepoOutcome = Result<(String, Option<crate::share::FilePlacement>), String>;

/// Extended work item: repo path + manifest entry + old tip SHA captured
/// before the advance loop runs.
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
    filter: &RepoFilter,
    jobs: usize,
    opts: crate::activate::ActivateOptions,
) -> anyhow::Result<()> {
    let active_root = ctx.active_path();
    let primary_root = ctx.primary_path();
    let project_dir = crate::workspace::project_dir(active_root, project_name);
    let project = Project::from_dir(&project_dir, project_name.clone())
        .with_context(|| format!("failed to load project '{}'", project_name))?;
    crate::branch::require_recorded(ctx)?;
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        crate::shared_members::refuse_moving(primary_root, project_name, &|path, role| {
            filter.matches(path, role)
        })?;
    }

    if commit.is_some() {
        let authored = if filter.is_empty() {
            crate::activate::owned_paths(active_root, project_name, &project.manifest)
        } else {
            BTreeSet::new()
        };
        lock::judge_unrelated_dirt(
            crate::vcs::project_vcs().as_ref(),
            project_name,
            &project_dir,
            &authored,
        )?;
    }

    // Snapshot the repo list into a Vec so the parallel loop can index by
    // position. The BTreeMap iteration is deterministic, so the resulting
    // Vec mirrors the previous serial loop's order exactly.
    //
    // Apply the `--role` / `--repo` filter so only selected repos are
    // advanced. Empty filter is a no-op (every repo passes). The post-loop
    // lock re-snapshot below still walks the *full* manifest, so unfiltered
    // repos remain at their previous lock SHAs — see the comment by the
    // `lock::lock` call.
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
            WorkItem {
                repo_path: rp.clone(),
                entry: entry.clone(),
                checkout,
                old,
                vcs,
                planned: None,
            }
        })
        .collect();
    let work_items = plan_url_remotes(work_items)?;

    let parallel = jobs > 1;
    // Under JSON mode we suppress text-mode prefix output so stdout only
    // carries structured data. Text mode uses the Reporter prefix as usual.
    let use_reporter = output == OutputMode::Text;
    let write_lock: Mutex<()> = Mutex::new(());

    let outcomes: Vec<RepoOutcome> = run_in_parallel(&work_items, jobs, |_idx, item| {
        let reporter = if parallel && use_reporter {
            Reporter::parallel(item.repo_path.as_str().to_string(), &write_lock)
        } else {
            Reporter::serial()
        };
        advance_one(
            item.vcs.as_ref(),
            &item.repo_path,
            &item.entry,
            item.planned.as_ref(),
            &item.checkout,
            &reporter,
            use_reporter,
        )
        .and_then(|new_sha| {
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

    // Re-snapshot the lock to capture the new tips. Delegates to the same
    // `lock::lock` entry point so the commit/dirty handling, hook fire
    // policy, and error surface stay consistent. Pass the same override
    // through so the lock operates on the same project the update did.
    //
    // Critical: this happens AFTER the parallel worker pool has joined.
    // The lock file is shared project-wide state; concurrent writes would
    // race. Keeping it serial post-join is the natural fit for the
    // existing structure.
    //
    // Filter scope: the `--role` / `--repo` filter narrows the *advance*
    // loop above, not the lock snapshot. `lock::lock` walks the full
    // manifest and records HEAD of every repo on disk: filtered repos are
    // at their newly-advanced HEAD; unfiltered repos are at whatever HEAD
    // they were already on. This preserves the invariant that the lock
    // always describes the whole manifest. The filter narrows the loop,
    // not the lock-shape — same decision as push in `src/push.rs`.
    let pending_commit = lock::write_project_lock(ctx, dirty, commit)
        .context("failed to write lock after update")?;

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

/// Per-repo worker: the pruning fetch of every remote, resolve the branch on
/// the member's url remote, then advance the checkout onto it
/// ([`advance_checkout`]).
///
/// Returns `Ok(new_sha)` on success (the SHA the repo is now at) or
/// `Err(msg)` on failure. `use_reporter` suppresses progress lines under
/// `--json` so stdout carries only structured data.
///
/// All user-facing text output is routed through `reporter`, which prefixes
/// `[<repo>]` and serializes writes under `-j > 1`; under `-j 1` the
/// reporter is a no-prefix passthrough.
#[allow(clippy::too_many_arguments)]
fn advance_one(
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    planned: Option<&crate::url_remote::Planned>,
    checkout: &MemberCheckout,
    reporter: &Reporter<'_>,
    use_reporter: bool,
) -> Result<String, String> {
    let repo_dir = checkout.path();
    // Which checkout this member is advancing decides which ref is the legal
    // object of the MOVE — the canonical's tracking counterpart, or the
    // workweave's ephemeral ref.
    let in_workweave = checkout.is_workweave_slot();

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

    // Resolve the branch HEAD on the conventional remote. The VCS layer
    // owns the naming policy, so this is one call rather than a fallback
    // chain. No bare-branch fallback — missing-remote produces a clear
    // error rather than silently resolving to the local branch tip.
    //
    // When `--prune` above removed a stale remote-tracking ref for
    // `branch`, this resolution will now correctly fail rather than
    // resolving against a ghost ref. The error below names the state and
    // the two actionable exits so the operator knows what to do.
    let resolved = match vcs.resolve_branch_on_remote(repo_dir, &planned.remote, &branch) {
        Ok(r) => r,
        Err(_) => {
            return Err(format!(
                "{repo}: branch '{branch}' does not resolve on the remote \
                 — renamed or deleted upstream, or `branch` was never a \
                 branch (it tracks a branch only; it cannot pin a tag or \
                 SHA). To fix: update rwv.toml's `branch` field to the \
                 current branch name.",
                repo = repo_path.as_str(),
                branch = branch.branch(),
            ));
        }
    };

    advance_checkout(vcs, repo_path, entry, repo_dir, in_workweave, &resolved)?;

    // Capture the new SHA after checkout for JSON reporting.
    let new_sha = vcs
        .head_revision(repo_dir)
        .ok()
        .map(|r| r.display_str().to_owned())
        .unwrap_or_else(|| resolved.to_string());

    Ok(new_sha)
}

/// Advance one checkout onto `target`, the tracking branch's tip on the
/// member's url remote.
///
/// A checkout attached to the tracking declaration's local counterpart is
/// fast-forwarded; one that is not fast-forwardable refuses, naming the
/// reconcile. A checkout attached to anything else refuses, naming `rwv
/// update`. Inside a workweave the ref the checkout is on advances when
/// `target` is a fast-forward, and `rwv sync` is named otherwise. A detached
/// HEAD moves, under the mid-operation precondition of
/// [`Vcs::advance_detached_head`]. An unborn branch refuses.
fn advance_checkout(
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    repo_dir: &Path,
    in_workweave: bool,
    target: &ResolvedRevisionId,
) -> Result<(), String> {
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
    let is_fast_forward =
        &head == target || vcs.is_ancestor(repo_dir, &head, target).map_err(describe)?;

    if in_workweave {
        if &head == target {
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

    let counterpart = entry.branch.local_counterpart();

    if !attached.is_named(&counterpart) {
        return Err(format!(
            "{}: is on branch '{attached}', not the local counterpart \
             ('{counterpart}') of the branch the manifest declares — \
             rwv update --remote moves only that branch, so it will not relocate one it \
             cannot relate to the tracking declaration.\n  \
             Run `rwv update` to align the member, then re-run.",
            repo_path.as_str(),
        ));
    }

    if &head == target {
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
