//! `rwv update` — advance each manifest repo to its branch HEAD and
//! re-snapshot `rwv.lock`.
//!
//! Semantically analogous to `cargo update` or `npm update`: this is the
//! verb that mutates the lock by pulling fresh tips from the network.
//! `rwv fetch` (default) reads the lock; `rwv lock` snapshots local tips;
//! `rwv update` advances and re-snapshots.

use crate::git::{git_command, GitVcs};
use crate::lock;
use crate::manifest::{Project, ProjectName, RepoEntry, RepoPath};
use crate::parallel::{run_in_parallel, run_subprocess_with_reporter, Reporter};
use crate::selector::RepoFilter;
use crate::vcs::{RefName, Vcs};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Schema URL for `rwv update --json` output. Pins to the committed artifact
/// under `docs/reference/schemas/update.json`.
pub const UPDATE_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/update.json";

/// Per-repo outcome kind for `rwv update --json`.
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

/// Per-repo record in `rwv update --json` output.
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
    pub absolute_path: String,
    /// Branch name from the manifest `version:` field.
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

/// Top-level envelope for `rwv update --json` (serial / `-j 1` mode).
/// `{ "$schema": "<url>", "repos": [<RepoUpdateRecord>, ...] }`
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub repos: Vec<RepoUpdateRecord>,
}

/// One NDJSON record emitted by `rwv update --json -j N` with `N > 1`.
///
/// Each record is a flat JSON object with `$schema` embedded so consumers
/// can identify it without out-of-band context.
#[derive(Debug, Serialize)]
struct UpdateNdjsonRecord<'a> {
    #[serde(rename = "$schema")]
    schema: &'a str,
    #[serde(flatten)]
    record: &'a RepoUpdateRecord,
}

/// Run `rwv update` for the current workspace context.
///
/// For each repo in the active project's manifest:
/// 1. `git fetch` the remote.
/// 2. Resolve the branch (`version:` in the manifest) on the remote — this
///    is the canonical "HEAD of the upstream branch" value.
/// 3. Checkout that revision in the local clone.
///
/// After all repos are advanced, regenerate `rwv.lock` from the new tips
/// and write it. The lock-write reuses `lock::generate_lock`, which carries
/// the dirty check; `dirty` here controls whether to bypass it.
///
/// When `commit` is true, the resulting lock is staged and committed in
/// the project repo (same semantics as `rwv lock --commit`).
/// When `project_override` is `Some`, that project is updated instead of
/// the active one (one-shot; does not change `.rwv-active`).
/// When `json` is true, structured output is emitted: an envelope under
/// `jobs == 1`, NDJSON under `jobs > 1`.
///
/// `jobs` is the resolved worker count (post-[`crate::parallel::resolve_jobs`]).
/// `jobs == 1` runs serially with no prefix; `jobs > 1` runs the per-repo
/// loop on a bounded worker pool, prefixing stdout/stderr lines with the
/// repo path. The lock write happens serially after all workers join.
pub fn run_update(
    cwd: &Path,
    dirty: bool,
    commit: bool,
    json: bool,
    project_override: Option<ProjectName>,
    filter: &RepoFilter,
    jobs: usize,
) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;

    // Cross-verb mutex (Correction 1, COVERAGE). `update` advances tips and
    // re-snapshots the lock in the active workspace; if an in-flight
    // `sync`/`sync-to` op involves that workspace (owner record or lease), its
    // half-applied state must not be perturbed. Refuse, naming the op and its
    // exits, via the SAME guard the sync engine uses — no new lease machinery.
    crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;

    let (project_name, workweave_name, workweave_dir) = match &ctx.location {
        WorkspaceLocation::Weave { .. } => {
            let name = ctx.require_active_project_on_disk()?.clone();
            (name, None, None)
        }
        WorkspaceLocation::Workweave { name, dir, project } => {
            (project.clone(), Some(name.clone()), Some(dir.clone()))
        }
    };

    update_for_project(
        ctx.active_path(),
        ctx.primary_path(),
        &project_name,
        workweave_name.as_ref().zip(workweave_dir.as_deref()),
        dirty,
        commit,
        json,
        project_override,
        filter,
        jobs,
    )
}

/// Outcome of advancing a single repo.
///
/// `Ok(new_sha)` means fetch + resolve + checkout ran cleanly and the repo
/// is now at `new_sha`. `Err(msg)` means one of those steps failed and
/// `msg` is the human-readable failure to surface in the aggregated summary.
type RepoOutcome = Result<String, String>;

/// Extended work item: repo path + manifest entry + old tip SHA captured
/// before the advance loop runs.
struct WorkItem {
    repo_path: RepoPath,
    entry: RepoEntry,
    absolute_path: PathBuf,
    /// HEAD SHA before the advance (`None` if unreadable — e.g. repo
    /// missing or git error).
    old_sha: Option<String>,
}

/// Internal: do the update for a specific project under `active_root`.
#[allow(clippy::too_many_arguments)]
fn update_for_project(
    active_root: &Path,
    primary_root: &Path,
    project_name: &ProjectName,
    workweave: Option<(&crate::manifest::WorkweaveName, &Path)>,
    dirty: bool,
    commit: bool,
    json: bool,
    project_override: Option<ProjectName>,
    filter: &RepoFilter,
    jobs: usize,
) -> anyhow::Result<()> {
    let project_dir = active_root.join("projects").join(project_name.as_str());
    let project = Project::from_dir(&project_dir)
        .with_context(|| format!("failed to load project '{}'", project_name))?;

    let git = GitVcs;

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
    let workweave_dir = workweave.map(|(_, wd)| wd);
    let work_items: Vec<WorkItem> = project
        .manifest
        .repositories
        .iter()
        .filter(|(rp, entry)| filter.matches(rp, entry.role))
        .map(|(rp, entry)| {
            let abs = resolve_repo_dir(rp, primary_root, workweave_dir);
            let old_sha = git
                .head_revision(&abs)
                .ok()
                .map(|r| r.display_str().to_owned());
            WorkItem {
                repo_path: rp.clone(),
                entry: entry.clone(),
                absolute_path: abs,
                old_sha,
            }
        })
        .collect();

    let parallel = jobs > 1;
    // Under JSON mode we suppress text-mode prefix output so stdout only
    // carries structured data. Text mode uses the Reporter prefix as usual.
    let use_reporter = !json;
    let write_lock: Mutex<()> = Mutex::new(());

    let outcomes: Vec<RepoOutcome> = run_in_parallel(&work_items, jobs, |_idx, item| {
        let reporter = if parallel && use_reporter {
            Reporter::parallel(item.repo_path.as_str().to_string(), &write_lock)
        } else {
            Reporter::serial()
        };
        advance_one(
            &git,
            &item.repo_path,
            &item.entry,
            primary_root,
            workweave_dir,
            &reporter,
            use_reporter,
        )
    });

    // Aggregate errors in input order — matches the existing serial shape.
    let mut errors: Vec<String> = Vec::new();
    let mut updated = 0usize;

    // Build JSON records if requested. We zip work_items with outcomes by
    // position (run_in_parallel preserves input order in its output).
    let ndjson = json && jobs > 1;
    let stdout_lock: Mutex<()> = Mutex::new(());
    let mut json_records: Vec<RepoUpdateRecord> = Vec::new();

    for (item, outcome) in work_items.iter().zip(outcomes) {
        let branch = item.entry.version.as_str().to_owned();
        let abs_str = item.absolute_path.to_string_lossy().to_string();

        let record = match outcome {
            Ok(new_sha) => {
                updated += 1;
                let kind = if item.old_sha.as_deref() == Some(new_sha.as_str()) {
                    UpdateKind::UpToDate
                } else {
                    UpdateKind::Updated
                };
                RepoUpdateRecord {
                    path: item.repo_path.to_string(),
                    absolute_path: abs_str,
                    branch,
                    kind,
                    old_sha: item.old_sha.clone(),
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
                    old_sha: item.old_sha.clone(),
                    new_sha: None,
                    error: Some(msg),
                }
            }
        };

        if json {
            if ndjson {
                // Stream one line per record to stdout as we build.
                let line_record = UpdateNdjsonRecord {
                    schema: UPDATE_SCHEMA_URL,
                    record: &record,
                };
                if let Ok(line) = serde_json::to_string(&line_record) {
                    let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
                    let stdout = std::io::stdout();
                    let mut handle = stdout.lock();
                    let _ = writeln!(handle, "{line}");
                    let _ = handle.flush();
                }
            }
            json_records.push(record);
        }
    }

    if !errors.is_empty() && !json {
        eprintln!("rwv update: {} repo(s) failed to update:", errors.len());
        for msg in &errors {
            eprintln!("  - {msg}");
        }
        anyhow::bail!(
            "update aborted with {} failure(s); lock not written",
            errors.len()
        );
    }

    if !json {
        println!("rwv update: advanced {updated} repo(s)");
    }

    // Under JSON mode with failures: emit output first, then bail.
    // Under text mode with failures: already bailed above.
    // Under JSON mode without failures: fall through to lock write.
    if json && !errors.is_empty() {
        // Emit envelope (NDJSON already streamed above).
        if !ndjson {
            let envelope = UpdateJsonOutput {
                schema_url: UPDATE_SCHEMA_URL.to_string(),
                repos: json_records,
            };
            let out = serde_json::to_string_pretty(&envelope)
                .context("failed to serialize update output")?;
            println!("{out}");
        }
        anyhow::bail!(
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
    let _ = workweave; // suppress unused warning if generate_lock signature changes
    lock::lock(active_root, dirty, commit, project_override)
        .context("failed to write lock after update")?;

    // Emit JSON envelope after lock write (so the lock is coherent before
    // consumers read the envelope). NDJSON was already streamed above.
    if json && !ndjson {
        let envelope = UpdateJsonOutput {
            schema_url: UPDATE_SCHEMA_URL.to_string(),
            repos: json_records,
        };
        let out =
            serde_json::to_string_pretty(&envelope).context("failed to serialize update output")?;
        println!("{out}");
    }

    Ok(())
}

/// Resolve the on-disk path for a repo, preferring the workweave overlay
/// when the repo exists there, falling back to `primary_root`.
fn resolve_repo_dir(
    repo_path: &RepoPath,
    primary_root: &Path,
    workweave_dir: Option<&Path>,
) -> PathBuf {
    if let Some(wd) = workweave_dir {
        let candidate = wd.join(repo_path.as_path());
        if candidate.exists() {
            return candidate;
        }
    }
    primary_root.join(repo_path.as_path())
}

/// Per-repo worker: `git fetch --all --tags`, resolve the role-conventional
/// remote branch, then check out the resolved revision.
///
/// Returns `Ok(new_sha)` on success (the SHA the repo is now at) or
/// `Err(msg)` on failure. `use_reporter` suppresses progress lines under
/// `--json` so stdout carries only structured data.
///
/// All user-facing text output is routed through `reporter`, which prefixes
/// `[<repo>]` and serialises writes under `-j > 1`; under `-j 1` the
/// reporter is a no-prefix passthrough.
fn advance_one(
    git: &GitVcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    primary_root: &Path,
    workweave_dir: Option<&Path>,
    reporter: &Reporter<'_>,
    use_reporter: bool,
) -> RepoOutcome {
    let repo_dir = resolve_repo_dir(repo_path, primary_root, workweave_dir);

    if !repo_dir.exists() {
        // `rwv fetch` (no SOURCE) re-clones missing members of the active
        // project — the settled repair verb for dangling references. See
        // fetch::run_fetch_in_place.
        return Err(format!(
            "{}: clone missing on disk; run `rwv fetch` from the workspace \
             to re-materialize missing manifest members, then re-run `rwv update`",
            repo_path.as_str(),
        ));
    }

    let branch = entry.version.as_str();

    // git fetch the remote(s). Run from the repo dir so default remote
    // selection applies.
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
        reporter.out(&format!("rwv update: fetching {}", repo_path.as_str()));
    }
    let mut cmd = git_command();
    cmd.args(["fetch", "--all", "--tags", "--prune"])
        .current_dir(&repo_dir);
    let outcome = match run_subprocess_with_reporter(&mut cmd, reporter) {
        Ok(o) => o,
        Err(e) => {
            return Err(format!(
                "{}: git fetch failed to spawn: {e}",
                repo_path.as_str()
            ));
        }
    };
    if !outcome.status.success() {
        // Under serial mode `stderr_capture` carries git's stderr; under
        // parallel mode the stderr was already streamed through the
        // reporter, so the captured string is empty and we just say
        // "failed". The streamed lines remain on the terminal for the
        // user to read.
        let suffix = if outcome.stderr_capture.is_empty() {
            "failed".to_string()
        } else {
            format!("failed: {}", outcome.stderr_capture.trim())
        };
        return Err(format!("{}: git fetch {suffix}", repo_path.as_str()));
    }

    // Resolve the branch HEAD on the role-conventional remote. The VCS
    // layer owns the per-role naming policy, so this is one call rather
    // than a fallback chain. No bare-branch fallback —
    // missing-remote produces a clear error rather than silently
    // resolving to the local branch tip.
    //
    // When `--prune` above removed a stale remote-tracking ref for
    // `branch`, this resolution will now correctly fail rather than
    // resolving against a ghost ref. The error below names the state and
    // the two actionable exits so the operator knows what to do.
    let branch_ref = RefName::new(branch);
    let resolved = match git.resolve_branch_on_remote(&repo_dir, entry.role, &branch_ref) {
        Ok(r) => r,
        Err(_) => {
            return Err(format!(
                "{repo}: branch '{branch}' no longer exists on the remote \
                 — deleted or renamed upstream. \
                 To fix: either update rwv.yaml's `version:` field to the \
                 new branch name, or pin `version:` to a SHA or tag.",
                repo = repo_path.as_str(),
            ));
        }
    };

    if let Err(e) = git.checkout(&repo_dir, &resolved) {
        return Err(format!(
            "{}: failed to checkout {}: {e}",
            repo_path.as_str(),
            resolved
        ));
    }

    // Capture the new SHA after checkout for JSON reporting.
    let new_sha = git
        .head_revision(&repo_dir)
        .ok()
        .map(|r| r.display_str().to_owned())
        .unwrap_or_else(|| resolved.to_string());

    Ok(new_sha)
}
