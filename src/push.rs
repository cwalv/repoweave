//! `rwv push` — coordinated cross-repo push.
//!
//! The publisher-side counterpart to `rwv update`. Pushes manifest repos
//! first, then the project repo last — the project repo carries the
//! committed lock that pins manifest SHAs, so collaborators' `rwv fetch`
//! must never see a committed lock referencing unpushed manifest commits.

use crate::manifest::{Project, ProjectName, RepoEntry, RepoPath, Role, VcsType};
use crate::parallel::{run_in_parallel, Reporter};
use crate::selector::RepoFilter;
use crate::vcs::{
    project_vcs, vcs_for, HeadAttachment, PublishRef, RawRefName, RawRevisionId, TrackingRef, Vcs,
};
use crate::workspace::{Checkout, Resolution, WorkspaceContext};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Schema URL for `rwv push --json` output. Pins to the committed artifact
/// under `docs/reference/schemas/push.json`. Emitted as the top-level
/// `$schema` field of the [`PushJsonOutput`] envelope and in every NDJSON
/// record under `--json -j N` with `N > 1`.
pub const PUSH_SCHEMA_URL: &str = crate::schema_url::schema_url!("push");

// ---------------------------------------------------------------------------
// JSON wire-output types for `rwv push --json`
// ---------------------------------------------------------------------------

/// One per-repo outcome record in `rwv push --json` output.
///
/// Manifest-repo records use `kind` values `pushed`, `skipped`, and `failed`.
/// The project-repo record uses `kind` values `project-repo-pushed` and
/// `project-repo-failed`, making it distinguishable from manifest-repo records
/// in the same flat `outcomes` array.
///
/// Choosing option (a) — a `kind` field — over option (b) (two separate
/// arrays) because: a single flat array supports uniform streaming in NDJSON
/// mode without requiring consumers to merge two streams; the `kind` tag
/// already carries all the type information consumers need; and the kebab-case
/// kind-tag convention matches sync/status/doctor precedent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PushOutcomeOutput {
    /// Manifest repo was pushed successfully.
    Pushed { path: String, absolute_path: String },
    /// Manifest repo was skipped (e.g. by a selector filter).
    Skipped { path: String, absolute_path: String },
    /// Manifest repo push failed.
    Failed {
        path: String,
        absolute_path: String,
        /// Free-form error message from the git push attempt.
        message: String,
    },
    /// Project repo was pushed successfully (always the last record).
    ProjectRepoPushed {
        path: String,
        absolute_path: String,
        /// The project name (e.g. `"my-app"`). Distinguishes the project repo's
        /// path convention (`projects/<name>/`) from manifest-repo paths.
        project: String,
    },
    /// Project repo push failed.
    ProjectRepoFailed {
        path: String,
        absolute_path: String,
        project: String,
        /// Free-form error message from the git push attempt.
        message: String,
    },
}

impl PushOutcomeOutput {
    /// True when this record represents a failure (either manifest or project-repo).
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::ProjectRepoFailed { .. })
    }
}

/// Top-level envelope for `rwv push --json` (serial mode, `jobs == 1`).
///
/// Shape: `{ "$schema": "<url>", "outcomes": [<PushOutcomeOutput>, ...] }`.
/// Manifest-repo records appear first, in manifest order; the project-repo
/// record is appended last (reflecting push ordering). Consumers can
/// distinguish the project-repo record by checking `kind` for
/// `"project-repo-pushed"` or `"project-repo-failed"`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PushJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub outcomes: Vec<PushOutcomeOutput>,
    /// Resolved workspace coordinates (workspace root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// One NDJSON record for `rwv push --json -j N` with `N > 1`.
///
/// Each line is self-describing: the outcome's fields are flattened alongside
/// `$schema` so a consumer can identify any single line without context.
#[derive(Debug, Serialize)]
pub struct PushOutcomeNdjsonRecord<'a> {
    #[serde(rename = "$schema")]
    pub schema: &'a str,
    #[serde(flatten)]
    pub outcome: &'a PushOutcomeOutput,
}

/// Run `rwv push` for the current workspace context.
///
/// Refuses if invoked from a workweave (workweave branches shouldn't leak
/// to shared remotes). Refuses if the project repo is off its canonical
/// branch. Walks the manifest pushing writable repos (Owned + Fork by
/// default) on the currently-checked-out branch; aggregates failures and
/// only pushes the project repo if every manifest push succeeded.
///
/// When `filter` is empty (no `--role` / `--repo` selectors), the push loop
/// defaults to `[Owned, Fork]`. Dependency and Reference repos are skipped
/// with a one-line notice each. When `filter` is non-empty, the caller's
/// selectors are used verbatim — selectors override the default and can
/// include non-writable roles.
///
/// `force` propagates to every git push in the operation. `dry_run` prints
/// the plan without executing any pushes.
///
/// `json` enables structured output. Under `json && jobs == 1`, the result
/// is a pretty-printed [`PushJsonOutput`] envelope. Under `json && jobs > 1`,
/// each record is streamed as a self-describing NDJSON line as it completes;
/// the project-repo record is appended last. Text-mode chatter (the normal
/// "rwv push: pushing X" lines) is suppressed under `--json`.
///
/// `jobs` is the resolved worker count (post-[`crate::parallel::resolve_jobs`])
/// for the manifest-repo push loop. `jobs == 1` runs serially with no prefix;
/// `jobs > 1` fans the manifest-repo pushes out over a bounded worker pool,
/// prefixing per-repo lines with `[<repo-path>]`. The project-repo push always
/// runs serially as the last step, regardless of `jobs`.
pub fn run_push(
    ctx: &WorkspaceContext,
    dry_run: bool,
    force: bool,
    filter: &RepoFilter,
    jobs: usize,
    json: bool,
) -> anyhow::Result<()> {
    // 1. Workspace precondition — refuse from a workweave.
    if let Checkout::Workweave { name, .. } = &ctx.checkout {
        anyhow::bail!(
            "rwv push: refusing to push from workweave '{}'; \
             workweave branches shouldn't leak to shared remotes. \
             Run `rwv sync-to` from the workweave to land changes on the parent first.",
            name
        );
    }

    let project_name = ctx.require_active_project_on_disk()?.clone();
    let primary_root = ctx.primary_path().to_path_buf();
    let project_dir = primary_root.join("projects").join(project_name.as_str());

    let project = Project::from_dir(&project_dir)
        .with_context(|| format!("failed to load project '{}'", project_name))?;

    let project_vcs = project_vcs();

    // 2. Project repo must be on its canonical branch. Publishing is a
    //    gateway-to-collaboration operation; require a stable primary
    //    context so collaborators see a predictable target branch.
    //
    //    "Canonical branch" is the remote's own declaration
    //    (`RemoteDefaultBranch`) — not a fabricated default. `None` means
    //    `origin/HEAD` is unset, and the gate refuses rather than guessing
    //    "main". A non-repo project dir surfaces as `VcsError::NotARepo`
    //    here, before HEAD is ever read, instead of being misreported as a
    //    detached checkout.
    let project_remote_default = project_vcs
        .remote_default_branch(&project_dir)
        .with_context(|| format!("failed to determine canonical branch for {project_name}"))?;
    let Some(project_remote_default) = project_remote_default else {
        anyhow::bail!(
            "rwv push: project repo at projects/{project_name}/: origin/HEAD is unset; \
             run `git remote set-head origin -a` in the project repo (or push once with \
             an explicit branch) to record its canonical branch, then re-run `rwv push`"
        );
    };
    let project_canonical = project_remote_default.local_counterpart();

    let project_attachment = project_vcs
        .head_attachment(&project_dir)
        .with_context(|| format!("failed to read current branch for {project_name}"))?;
    let project_attached = match project_attachment {
        HeadAttachment::Attached(a) => a,
        HeadAttachment::Unborn(u) => anyhow::bail!(
            "rwv push: project repo at projects/{project_name}/ is on branch '{u}' with no \
             commits yet; make an initial commit before pushing"
        ),
        HeadAttachment::Detached(_) => anyhow::bail!(
            "rwv push: project repo at projects/{project_name}/ is on a detached HEAD; \
             check out the canonical branch ({project_canonical}) first"
        ),
    };
    if !project_attached.is_named(&project_canonical) {
        anyhow::bail!(
            "rwv push: project repo at projects/{project_name}/ is on branch '{project_attached}', \
             not the canonical branch '{project_canonical}'. \
             Switch to '{project_canonical}' before pushing — publishing requires a stable primary context."
        );
    }
    // The single decision site: `from_attached` publishes whatever the
    // checkout is on, never the manifest's declared branch.
    let project_publish_ref = PublishRef::from_attached(&project_attached);
    let project_current = project_attached.to_string();

    // Snapshot manifest repos into a Vec so we can iterate twice (precondition
    // checks + push loop) without re-walking the BTreeMap.
    let manifest_repos: Vec<(RepoPath, RepoEntry)> = project
        .manifest
        .repositories
        .iter()
        .map(|(rp, e)| (rp.clone(), e.clone()))
        .collect();

    // 3. Lock-matches-state precondition. Compare each manifest repo's HEAD
    //    SHA to the lock's recorded SHA. Bail before touching the network so
    //    we don't half-publish state the committed lock doesn't reference.
    //
    //    Filter scope: this check ALWAYS runs against the full manifest, even
    //    when `--role` / `--repo` narrows the actual push loop below. The
    //    committed lock describes every manifest repo; publishing a
    //    project-repo lock that doesn't match the unfiltered repos breaks
    //    collaborators' `rwv fetch` (they hit "object missing" against the
    //    pinned-but-unpublished SHAs). The filter narrows the push loop,
    //    not the precondition.
    let lock_path = project_dir.join("rwv.lock");
    let lock_entries: std::collections::BTreeMap<RepoPath, RawRevisionId> =
        if let Some(raw_lock) = &project.lock {
            raw_lock
                .iter_entries()
                .map(|(rp, entry)| (rp.clone(), entry.version.clone()))
                .collect()
        } else {
            std::collections::BTreeMap::new()
        };

    let mut lock_mismatches: Vec<String> = Vec::new();
    for (repo_path, entry) in &manifest_repos {
        let vcs = vcs_for(entry.vcs_type);
        let repo_dir = primary_root.join(repo_path.as_path());
        if !repo_dir.exists() {
            // `rwv fetch` (no SOURCE) re-clones missing members of the
            // active project — the settled repair verb for dangling
            // references. See fetch::run_fetch_in_place.
            lock_mismatches.push(format!(
                "{}: clone missing on disk; run `rwv fetch` from the workspace \
                 to re-materialize missing manifest members, then re-run `rwv push`",
                repo_path.as_str(),
            ));
            continue;
        }
        let head = match vcs.head_revision(&repo_dir) {
            Ok(h) => h,
            Err(e) => {
                lock_mismatches.push(format!(
                    "{}: failed to resolve HEAD: {e}",
                    repo_path.as_str()
                ));
                continue;
            }
        };
        let Some(lock_raw) = lock_entries.get(repo_path) else {
            lock_mismatches.push(format!(
                "{}: missing from {} — run `rwv lock` to record local state",
                repo_path.as_str(),
                lock_path.display()
            ));
            continue;
        };
        // Resolve the lock's raw version against the repo so tag-form lock
        // entries (e.g. `v1.2.3`) compare equal to a SHA-form HEAD.
        let lock_resolved = match vcs.resolve_revision(&repo_dir, lock_raw.as_str()) {
            Ok(r) => r,
            Err(e) => {
                lock_mismatches.push(format!(
                    "{}: lock entry '{}' could not be resolved in clone: {e}",
                    repo_path.as_str(),
                    lock_raw
                ));
                continue;
            }
        };
        if lock_resolved != head {
            lock_mismatches.push(format!(
                "{}: HEAD {} differs from lock {}",
                repo_path.as_str(),
                short_sha(head.as_str()),
                short_sha(lock_resolved.as_str()),
            ));
        }
    }

    if !lock_mismatches.is_empty() {
        eprintln!(
            "rwv push: {} repo(s) disagree with {}:",
            lock_mismatches.len(),
            lock_path.display()
        );
        for msg in &lock_mismatches {
            eprintln!("  - {msg}");
        }
        eprintln!(
            "Hint: run `rwv lock` to capture local state, \
             or `git checkout` in each repo to align with the lock."
        );
        anyhow::bail!("lock-state mismatch — refusing to push before clone state and lock agree");
    }

    // 4. Per-repo branch sanity. Detached HEAD is fatal; off-version branch
    //    is a warning (footgun catcher — operator may have a topic branch
    //    they intend to push; we warn but don't override).
    //
    //    Filter scope: the per-repo branch check runs only over the FILTERED
    //    subset — there's no reason to fail a filtered push because of a
    //    detached HEAD in a repo we aren't going to push. Contrast with the
    //    lock-precondition above, which runs over the full manifest because
    //    the *lock* describes the full manifest.

    // Plan-time default: when no --role / --repo selectors are supplied, limit
    // the push loop to writable roles (Owned + Fork). Dependency and Reference
    // repos return 403 against upstreams the operator doesn't own. When
    // selectors ARE passed, use them verbatim — selectors override the default.
    let (effective_filter, using_default): (RepoFilter, bool) = if filter.is_empty() {
        // No selectors supplied — default to Owned + Fork.
        (
            RepoFilter::parse(&["owned".to_string(), "fork".to_string()], &[])
                .expect("known-safe literal roles"),
            true,
        )
    } else {
        (filter.clone(), false)
    };

    let filtered_repos: Vec<&(RepoPath, RepoEntry)> = manifest_repos
        .iter()
        .filter(|(rp, entry)| effective_filter.matches(rp, entry.role))
        .collect();

    // When using the default filter, collect the repos that were excluded so
    // we can emit Skipped records and a user-visible skip notice.
    let default_skipped_repos: Vec<&(RepoPath, RepoEntry)> = if using_default {
        manifest_repos
            .iter()
            .filter(|(rp, entry)| !effective_filter.matches(rp, entry.role))
            .collect()
    } else {
        Vec::new()
    };
    let mut branch_errors: Vec<String> = Vec::new();
    let mut plan: Vec<PushPlanItem> = Vec::with_capacity(filtered_repos.len());
    for (repo_path, entry) in &filtered_repos {
        let vcs = vcs_for(entry.vcs_type);
        let repo_dir = primary_root.join(repo_path.as_path());
        let attachment = match vcs.head_attachment(&repo_dir) {
            Ok(a) => a,
            Err(e) => {
                branch_errors.push(format!(
                    "{}: failed to read current branch: {e}",
                    repo_path.as_str()
                ));
                continue;
            }
        };
        let attached = match &attachment {
            HeadAttachment::Attached(a) => a,
            HeadAttachment::Unborn(u) => {
                branch_errors.push(format!(
                    "{}: branch '{u}' has no commits yet — make an initial commit before pushing",
                    repo_path.as_str(),
                ));
                continue;
            }
            HeadAttachment::Detached(_) => {
                branch_errors.push(format!(
                    "{}: detached HEAD — checkout a branch before pushing",
                    repo_path.as_str()
                ));
                continue;
            }
        };
        // `entry.version` is the manifest's declared tracking branch, still
        // typed `RefName` (manifest.rs's migration to `TrackingRef` is
        // separate work — out of scope here). Route it through
        // `TrackingRef::parse` so the comparison below goes through
        // `local_counterpart()`, the same named projection the project
        // gate above uses, instead of a raw string compare.
        let declared = match TrackingRef::parse(RawRefName::new(entry.version.as_str())) {
            Ok(t) => t,
            Err(e) => {
                branch_errors.push(format!(
                    "{}: manifest declares an invalid tracking branch '{}': {e}",
                    repo_path.as_str(),
                    entry.version,
                ));
                continue;
            }
        };
        if !attached.is_named(&declared.local_counterpart()) {
            eprintln!(
                "rwv push: warning: {} is on branch '{}', manifest declares '{}'",
                repo_path.as_str(),
                attached,
                declared,
            );
        }
        plan.push(PushPlanItem {
            repo_path: repo_path.clone(),
            branch: attached.to_string(),
            role: entry.role,
            vcs_type: entry.vcs_type,
            publish_ref: PublishRef::from_attached(attached),
        });
    }
    if !branch_errors.is_empty() {
        eprintln!(
            "rwv push: {} repo(s) cannot be pushed:",
            branch_errors.len()
        );
        for msg in &branch_errors {
            eprintln!("  - {msg}");
        }
        anyhow::bail!("aborted before network — fix per-repo branch state and retry");
    }

    // 5. Dry-run: print the plan.
    if dry_run {
        println!("rwv push (dry-run):");
        for item in &plan {
            println!(
                "  {}: would push {} -> {}",
                item.repo_path.as_str(),
                item.branch,
                remote_label(item.role),
            );
        }
        for (repo_path, _) in &default_skipped_repos {
            println!(
                "  {}: skipped (non-writable role; pass --repo or --role to include)",
                repo_path.as_str(),
            );
        }
        println!(
            "  projects/{}: would push {} -> origin (last)",
            project_name, project_current,
        );
        return Ok(());
    }

    // 6. Manifest-repo push loop. Attempt all and collect — same shape as
    //    update.rs. Fans out over `jobs` workers when `jobs > 1`; per-line
    //    `[<repo>]` prefix under parallel mode, no prefix under `-j 1`.
    //
    //    Under `--json`, text-mode output (the per-repo "rwv push: pushing
    //    X" lines) is suppressed; the Reporter is always Serial/quiet so
    //    stdout stays clean for the JSON envelope or NDJSON records.
    let parallel = jobs > 1;
    let ndjson = crate::parallel::OutputMode::resolve(json, jobs).is_ndjson();
    let write_lock: Mutex<()> = Mutex::new(());

    // Collect (path, abs_path, outcome) triples for the JSON path.
    let plan_with_paths: Vec<(RepoPath, PathBuf, PushPlanItem)> = plan
        .into_iter()
        .map(|item| {
            let abs = primary_root.join(item.repo_path.as_path());
            let rp = item.repo_path.clone();
            (rp, abs, item)
        })
        .collect();

    // Re-form the bare plan slice for run_in_parallel.
    let plan_items: Vec<PushPlanItem> = plan_with_paths
        .iter()
        .map(|(_, _, item)| PushPlanItem {
            repo_path: item.repo_path.clone(),
            branch: item.branch.clone(),
            role: item.role,
            vcs_type: item.vcs_type,
            publish_ref: item.publish_ref.clone(),
        })
        .collect();

    // One handle per repo, resolved from its declared backend before the
    // fan-out. Workers index by the same position `run_in_parallel` hands
    // the item at, so nothing is resolved on a worker thread.
    let plan_vcs: Vec<Box<dyn Vcs>> = plan_items.iter().map(|i| vcs_for(i.vcs_type)).collect();

    let raw_outcomes: Vec<PushOutcome> = run_in_parallel(&plan_items, jobs, |idx, item| {
        let reporter = if json {
            // Under --json, suppress the text-mode chatter; records come out
            // via our own JSON/NDJSON emit after the loop. We use a silent
            // reporter so push_one's "rwv push: pushing X" lines don't
            // pollute the structured stdout stream.
            Reporter::silent()
        } else if parallel {
            Reporter::parallel(item.repo_path.as_str().to_string(), &write_lock)
        } else {
            Reporter::serial()
        };
        push_one(
            plan_vcs[idx].as_ref(),
            item,
            &primary_root,
            &reporter,
            force,
        )
    });

    // Build wire-output records (manifest repos only, in order).
    //
    // Prepend plan-time skipped records first (default-filter excluded repos),
    // then the pushed/failed outcomes from the parallel loop. This ordering
    // keeps the JSON wire shape uniform: all manifest records before the
    // project-repo record.
    let mut json_outcomes: Vec<PushOutcomeOutput> = Vec::new();
    let mut push_errors: Vec<String> = Vec::new();
    let mut pushed = 0usize;
    let mut skipped = 0usize;

    // Emit text + JSON records for plan-time skipped repos (default filter
    // excluded non-writable roles). Under --json, these become Skipped
    // records in the outcomes array / NDJSON stream.
    for (repo_path, _entry) in &default_skipped_repos {
        let path_str = repo_path.as_str().to_string();
        let abs_str = primary_root
            .join(repo_path.as_path())
            .to_string_lossy()
            .into_owned();
        if !json {
            println!("rwv push: skipped {} (non-writable role)", path_str,);
        }
        skipped += 1;
        let wire = PushOutcomeOutput::Skipped {
            path: path_str,
            absolute_path: abs_str,
        };
        if ndjson {
            emit_ndjson_record(&write_lock, &wire);
        }
        json_outcomes.push(wire);
    }

    for ((repo_path, abs_path, _item), raw) in plan_with_paths.iter().zip(raw_outcomes) {
        let path_str = repo_path.to_string();
        let abs_str = abs_path.to_string_lossy().into_owned();

        let wire = match &raw {
            PushOutcome::Pushed => {
                pushed += 1;
                PushOutcomeOutput::Pushed {
                    path: path_str,
                    absolute_path: abs_str,
                }
            }
            PushOutcome::Failed(msg) => {
                push_errors.push(msg.clone());
                PushOutcomeOutput::Failed {
                    path: path_str,
                    absolute_path: abs_str,
                    message: msg.clone(),
                }
            }
        };

        if ndjson {
            // Stream each manifest-repo outcome immediately.
            emit_ndjson_record(&write_lock, &wire);
        }
        json_outcomes.push(wire);
    }

    if !push_errors.is_empty() {
        if !json {
            eprintln!(
                "rwv push: {}/{} manifest repo(s) failed; project repo not pushed:",
                push_errors.len(),
                plan_items.len(),
            );
            for msg in &push_errors {
                eprintln!("  - {msg}");
            }
        }

        // Under --json, emit whatever outcomes we have before bailing so the
        // caller gets machine-readable failure records.
        if json && !ndjson {
            let envelope = PushJsonOutput {
                schema_url: PUSH_SCHEMA_URL.to_string(),
                outcomes: json_outcomes,
                resolution: ctx.resolution(),
            };
            if let Ok(out) = serde_json::to_string_pretty(&envelope) {
                println!("{out}");
            }
        }

        anyhow::bail!(
            "manifest-repo push failures aborted before project-repo push; \
             manifest-side partial state may exist — inspect and retry"
        );
    }

    if !json {
        if skipped > 0 {
            println!(
                "rwv push: pushed {} manifest repo(s); {} skipped (non-writable role; pass --repo or --role to include)",
                pushed, skipped
            );
        } else {
            println!("rwv push: pushed {} manifest repo(s)", pushed);
        }
    }

    // 7. Project-repo push (gated). The project repo's committed lock pins
    //    the manifest SHAs we just pushed — pushing it last preserves the
    //    invariant that the remote-side lock never references unpushed
    //    objects. Use the same trait method so role policy stays in one
    //    place; the project repo is always Role::Owned at the trait
    //    layer (it's the canonical-tip carrier; not declared in any
    //    manifest).
    let project_path_str = format!("projects/{}", project_name.as_str());
    let project_abs_str = project_dir.to_string_lossy().into_owned();

    if !json {
        println!(
            "rwv push: pushing project repo projects/{} ({} -> origin)",
            project_name, project_current,
        );
    }
    let project_push_result =
        project_vcs.push_ref(&project_dir, Role::Owned, &project_publish_ref, force);

    let project_wire = match &project_push_result {
        Ok(()) => PushOutcomeOutput::ProjectRepoPushed {
            path: project_path_str,
            absolute_path: project_abs_str,
            project: project_name.as_str().to_string(),
        },
        Err(e) => PushOutcomeOutput::ProjectRepoFailed {
            path: project_path_str,
            absolute_path: project_abs_str,
            project: project_name.as_str().to_string(),
            message: e.to_string(),
        },
    };

    if ndjson {
        emit_ndjson_record(&write_lock, &project_wire);
    }
    json_outcomes.push(project_wire);

    if let Err(e) = project_push_result {
        // Under --json (envelope mode), emit partial outcomes before bailing.
        if json && !ndjson {
            let envelope = PushJsonOutput {
                schema_url: PUSH_SCHEMA_URL.to_string(),
                outcomes: json_outcomes,
                resolution: ctx.resolution(),
            };
            if let Ok(out) = serde_json::to_string_pretty(&envelope) {
                println!("{out}");
            }
        }
        anyhow::bail!(
            "project-repo push failed after all manifest repos pushed cleanly: {e}. \
             Manifest-side state is published; the lock carrier is not. \
             Retry `rwv push` once the project-repo issue is resolved."
        );
    }

    // Emit JSON output for the success path.
    if json {
        if !ndjson {
            // Envelope mode: emit once at the end.
            let envelope = PushJsonOutput {
                schema_url: PUSH_SCHEMA_URL.to_string(),
                outcomes: json_outcomes,
                resolution: ctx.resolution(),
            };
            let out = serde_json::to_string_pretty(&envelope)
                .context("failed to serialize push outcomes to JSON")?;
            println!("{out}");
        }
        // NDJSON mode: already streamed each record above.
    } else {
        println!("rwv push: done");
    }

    Ok(())
}

/// Emit one NDJSON record to stdout, serialised and mutex-guarded so
/// parallel workers cannot interleave bytes.
fn emit_ndjson_record(write_lock: &Mutex<()>, outcome: &PushOutcomeOutput) {
    let record = PushOutcomeNdjsonRecord {
        schema: PUSH_SCHEMA_URL,
        outcome,
    };
    if let Ok(line) = serde_json::to_string(&record) {
        let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        let _ = writeln!(handle, "{line}");
        let _ = handle.flush();
    }
}

/// One manifest repo's resolved plan entry — what branch it's on and what
/// role drives the remote selection. Built up-front so the dry-run output
/// and the actual push loop share the same shape.
///
/// `publish_ref` is the typed ref `push_one` hands to `Vcs::push_ref` —
/// computed once, at plan time, from the same `AttachedRef` witness
/// `branch` was rendered from. There is no independent re-read at push time,
/// so the plan reports and pushes the same ref even if the checkout moves
/// under it.
struct PushPlanItem {
    repo_path: RepoPath,
    branch: String,
    role: Role,
    vcs_type: VcsType,
    publish_ref: PublishRef,
}

/// Outcome of pushing a single manifest repo.
///
/// `Pushed` and `Failed` are produced by `push_one` for each plan item.
/// Plan-time skipped repos (filtered out by the default `[Owned, Fork]`
/// scope, or by explicit selectors) never enter the push loop and never
/// produce a `PushOutcome`; they are handled separately as
/// `PushOutcomeOutput::Skipped` records in the JSON output and as text
/// skip notices in text mode.
enum PushOutcome {
    Pushed,
    Failed(String),
}

/// Per-repo worker: push one manifest entry on its current branch via the
/// role-conventional remote (`origin` for all roles). All user-facing output
/// is routed through `reporter`, which prefixes `[<repo>]` and serialises
/// writes under `-j > 1`; under `-j 1` the reporter is a no-prefix
/// passthrough that matches the pre-`-j` serial output exactly.
///
/// `Vcs::push_ref` captures stdout/stderr; we don't stream git's output
/// line-by-line. The user-visible signal under parallel mode is the
/// pre/post "rwv push: pushing X" pair (lock-protected via reporter); on
/// failure the captured stderr is surfaced through the aggregated error
/// summary post-join.
fn push_one(
    vcs: &dyn Vcs,
    item: &PushPlanItem,
    primary_root: &Path,
    reporter: &Reporter<'_>,
    force: bool,
) -> PushOutcome {
    let repo_dir = primary_root.join(item.repo_path.as_path());
    reporter.out(&format!(
        "rwv push: pushing {} ({} -> {})",
        item.repo_path.as_str(),
        item.branch,
        remote_label(item.role),
    ));
    match vcs.push_ref(&repo_dir, item.role, &item.publish_ref, force) {
        Ok(()) => PushOutcome::Pushed,
        Err(e) => PushOutcome::Failed(format!("{}: git push failed: {e}", item.repo_path.as_str())),
    }
}

/// Display the remote name for a role — all roles push to `origin`.
fn remote_label(role: Role) -> &'static str {
    let _ = role;
    "origin"
}

/// Abbreviate a SHA to 7 chars (matches lock-commit-message convention).
/// Pass-through for non-SHA strings (already abbreviated, tag-form, etc.).
fn short_sha(s: &str) -> String {
    if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        s[..7].to_string()
    } else {
        s.to_string()
    }
}

/// Resolve the project repo's path for tests that need the on-disk dir.
/// Kept `pub(crate)` so the integration tests in `tests/push_test.rs`
/// can mirror primary's layout when staging fixtures.
#[allow(dead_code)]
pub(crate) fn project_repo_dir(primary_root: &Path, project: &ProjectName) -> PathBuf {
    primary_root.join("projects").join(project.as_str())
}

#[cfg(test)]
mod tests {
    //! Unit tests for the small private helpers `push.rs` uses to format
    //! its output. These helpers are only reachable through the verb's
    //! integration tests; the unit tests below pin them directly so
    //! refactors of either helper can't quietly change the verb's
    //! user-visible strings.
    use super::*;
    use crate::manifest::RepoPath;
    use crate::vcs::RawRevisionId;
    use std::collections::BTreeMap;

    // --- remote_label ------------------------------------------------------

    #[test]
    fn remote_label_owned_is_origin() {
        assert_eq!(remote_label(Role::Owned), "origin");
    }

    #[test]
    fn remote_label_dependency_is_origin() {
        assert_eq!(remote_label(Role::Dependency), "origin");
    }

    #[test]
    fn remote_label_reference_is_origin() {
        assert_eq!(remote_label(Role::Reference), "origin");
    }

    /// Fork is now treated identically to Owned — both push to `origin`.
    #[test]
    fn remote_label_fork_is_origin() {
        assert_eq!(remote_label(Role::Fork), "origin");
    }

    /// remote_label returns `&'static str` — no allocation per call.
    /// (Compile-time check via the function signature; this test just
    /// keeps the intent grep-discoverable.)
    #[test]
    fn remote_label_returns_static_str() {
        let _s: &'static str = remote_label(Role::Owned);
    }

    // --- short_sha ---------------------------------------------------------

    /// A canonical 40-char hex SHA abbreviates to its 7-char prefix —
    /// the same convention the lock-mismatch error message uses, and
    /// the same default `git log --oneline` uses.
    #[test]
    fn short_sha_truncates_40_hex_to_7() {
        let sha = "abcdef0123456789abcdef0123456789abcdef01";
        assert_eq!(sha.len(), 40);
        assert_eq!(short_sha(sha), "abcdef0");
    }

    #[test]
    fn short_sha_uppercase_hex_truncates_too() {
        // is_ascii_hexdigit accepts both cases; we exercise that path.
        let sha = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        assert_eq!(short_sha(sha), "ABCDEF0");
    }

    /// Non-40-char strings are passed through untouched — already
    /// abbreviated SHAs (e.g. 12 chars), tag-form lock entries
    /// (`v1.2.3`), and human-friendly labels alike.
    #[test]
    fn short_sha_passes_through_short_input() {
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha("abc1234"), "abc1234");
        assert_eq!(short_sha(""), "");
    }

    #[test]
    fn short_sha_passes_through_tag_form() {
        // `v1.2.3` is a legitimate lock-entry value when HEAD is tagged.
        // It is not 40 hex chars; short_sha must not mangle it.
        assert_eq!(short_sha("v1.2.3"), "v1.2.3");
    }

    /// A 40-char string that is *not* hex (one non-hex character) must
    /// be passed through — short_sha must never silently truncate a
    /// non-SHA blob to "look like" a SHA.
    #[test]
    fn short_sha_passes_through_40_char_non_hex() {
        // 40 chars, one is `z` (non-hex).
        let s = "zbcdef0123456789abcdef0123456789abcdef01";
        assert_eq!(s.len(), 40);
        assert_eq!(short_sha(s), s);
    }

    /// A 41-char hex string is *not* a git SHA (the upstream length is
    /// always 40 for SHA-1); pass through.
    #[test]
    fn short_sha_passes_through_41_char_hex() {
        let s = "abcdef0123456789abcdef0123456789abcdef011";
        assert_eq!(s.len(), 41);
        assert_eq!(short_sha(s), s);
    }

    // --- PushPlanItem round-trip ------------------------------------------

    /// Build a `PublishRef` for a test fixture without a real repo.
    ///
    /// `PublishRef::from_attached` needs an `AttachedRef` witness, which
    /// only `Vcs::head_attachment` can produce (its fields are private
    /// outside `vcs.rs`). `from_local` is the other constructor, which the
    /// gate does not call — reaching for it here to fabricate a test value
    /// doesn't touch the gate's decision, it just needs *a* value.
    fn test_publish_ref(name: &str) -> PublishRef {
        let declared = TrackingRef::parse(RawRefName::new(name)).expect("known-safe literal");
        PublishRef::from_local(&declared.local_counterpart())
    }

    /// PushPlanItem is the in-memory shape we hand to the parallel push
    /// loop and to dry-run printing. Pin construction-via-fields-and-
    /// readback so an accidental field rename doesn't silently change
    /// the dry-run output format.
    #[test]
    fn push_plan_item_round_trip_fields_readable() {
        let item = PushPlanItem {
            repo_path: RepoPath::new("github/cwalv/repoweave").expect("known-safe literal"),
            branch: "main".to_string(),
            role: Role::Owned,
            vcs_type: VcsType::Git,
            publish_ref: test_publish_ref("main"),
        };
        assert_eq!(item.repo_path.as_str(), "github/cwalv/repoweave");
        assert_eq!(item.branch, "main");
        assert_eq!(item.role, Role::Owned);
    }

    /// Sorted iteration is what the verb actually does — pin the
    /// shape we hand into `run_in_parallel`. (Selector-filtered
    /// subset preserves manifest order; the manifest is a BTreeMap.)
    #[test]
    fn push_plan_item_vec_preserves_order() {
        let plan = [
            PushPlanItem {
                repo_path: RepoPath::new("a").expect("known-safe literal"),
                branch: "main".into(),
                role: Role::Owned,
                vcs_type: VcsType::Git,
                publish_ref: test_publish_ref("main"),
            },
            PushPlanItem {
                repo_path: RepoPath::new("b").expect("known-safe literal"),
                branch: "main".into(),
                role: Role::Fork,
                vcs_type: VcsType::Git,
                publish_ref: test_publish_ref("main"),
            },
            PushPlanItem {
                repo_path: RepoPath::new("c").expect("known-safe literal"),
                branch: "main".into(),
                role: Role::Dependency,
                vcs_type: VcsType::Git,
                publish_ref: test_publish_ref("main"),
            },
        ];
        let labels: Vec<&str> = plan.iter().map(|i| remote_label(i.role)).collect();
        assert_eq!(labels, vec!["origin", "origin", "origin"]);
        let paths: Vec<&str> = plan.iter().map(|i| i.repo_path.as_str()).collect();
        assert_eq!(paths, vec!["a", "b", "c"]);
    }

    // --- defensive: lock-mismatch message shape stays stable ---------------

    /// The lock-mismatch error line that drives the recovery how-to in
    /// docs/how-to/push-cross-repo-feature.md uses `short_sha`
    /// directly. Spell out the composition so a contributor can search
    /// for the message shape from a doc snippet.
    #[test]
    fn lock_mismatch_uses_short_sha_for_both_sides() {
        let head = "1111111111111111111111111111111111111111";
        let lock = "2222222222222222222222222222222222222222";
        let msg = format!(
            "{}: HEAD {} differs from lock {}",
            "github/x/y",
            short_sha(head),
            short_sha(lock),
        );
        assert_eq!(msg, "github/x/y: HEAD 1111111 differs from lock 2222222");
    }

    /// The `BTreeMap<RepoPath, RawRevisionId>` shape used by the
    /// lock-precondition block is part of the verb's contract with
    /// `project.lock.repositories`. Spot-check the round-trip via a
    /// throwaway map so a future refactor of `RawRevisionId` would
    /// break this test before it broke the verb.
    #[test]
    fn lock_entries_btreemap_round_trip() {
        let mut entries: BTreeMap<RepoPath, RawRevisionId> = BTreeMap::new();
        entries.insert(
            RepoPath::new("github/x/y").expect("known-safe literal"),
            RawRevisionId::new("v1.0.0"),
        );
        entries.insert(
            RepoPath::new("github/a/b").expect("known-safe literal"),
            RawRevisionId::new("abcdef0123456789abcdef0123456789abcdef01"),
        );
        // BTreeMap orders by key — manifest order, lexicographic.
        let keys: Vec<&str> = entries.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["github/a/b", "github/x/y"]);
        // Values stay intact through insertion.
        assert_eq!(
            entries
                .get(&RepoPath::new("github/x/y").expect("known-safe literal"))
                .unwrap()
                .as_str(),
            "v1.0.0"
        );
    }
}
