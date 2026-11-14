//! `rwv status` — per-repo state of the CWD workspace.

use crate::alignment::{Accounting, Aligner, Member, ProjectStanding};
use crate::manifest::{Project, ProjectName, Role};
use crate::op_state;
use crate::parallel::OutputMode;
use crate::path_spelling::WirePath;
use crate::selector::RepoFilter;
use crate::vcs::{vcs_for, CommitId, MidOperation, ResolvedRevisionId, Vcs};
use crate::workspace::{project_dir, Checkout, Resolution, WorkspaceContext};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Schema URL for `rwv status --json` output. Pins to the committed artifact
/// under `docs/reference/schemas/status.json`. Emitted as the top-level
/// `$schema` field of the [`StatusJsonOutput`] envelope.
pub const STATUS_SCHEMA_URL: &str = crate::schema_url::schema_url!("status");

/// Top-level envelope for `rwv status --json`. Matches the convention adopted
/// by doctor (`$schema` + `violations`) and sync (`$schema` + `outcomes`):
/// `{ "$schema": "<url>", "repos": [<RepoStatus>, ...] }`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StatusJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub repos: Vec<RepoStatus>,
    /// Projects whose `rwv.toml` or `rwv.lock` did not load: none of their
    /// repos is in `repos`.
    pub unreadable_projects: Vec<UnreadableProject>,
    /// Each project's checked-out branch with its record and the branches that
    /// land into it, read in memory from the records as last fetched. A
    /// project whose repo is not on a branch has none.
    pub records: Vec<crate::branch::BranchRecordStatus>,
    /// The branch each project's workspace last aligned its members for.
    pub aligned: Vec<AlignedStatus>,
    /// Each repo judged on the axes `rwv status` reads, with that state's
    /// choices: `alignment` (a member not in its expected state, or carrying a
    /// fact such as `checkout`, and the project repo against the aligned
    /// branch), and `push-branch` where the branch needs attention: behind,
    /// diverged, gone with commits of its own, with a foreign upstream or with
    /// no push target. A branch that is ahead of its push branch is the
    /// ordinary state of work in progress and has no line; the per-repo
    /// `relation` and `operator_branches` carry what is unpublished. Empty,
    /// not absent, when none stands apart.
    pub judged: Vec<crate::diagnosis::JudgedOutput>,
    /// Each member's local branch holding unpushed commits that nothing else
    /// accounts for: checked out nowhere, named after no recorded weave
    /// branch of any project here, and no declared branch of that member.
    pub operator_branches: Vec<OperatorBranch>,
    /// The backups each repo holds, with what each keeps that no branch
    /// reaches. Only repos that hold one.
    pub backups: Vec<crate::backup::RepoBackupsOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// The sync/land op parked at this workspace, or leased to one
    /// elsewhere, if any. Absent when no op is in progress — the same
    /// disclosure the in-flight refusal makes, before an operator has to
    /// attempt a mutation to learn it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<OpStatus>,
    /// Why the operation record this workspace holds, or the one its lease
    /// names, cannot be read, as the refusal every other verb prints says it,
    /// naming the verb that ends the operation. Absent when there is no such
    /// record or it reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable_op: Option<String>,
}

/// What a project's workspace last aligned its members for.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AlignedStatus {
    pub project: ProjectName,
    /// The branch it last aligned for; absent where none is recorded or the
    /// workspace is detached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Whether the workspace was made detached.
    pub detached: bool,
    /// Why the workspace state, which reads as absent, could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
}

/// A member's local branch holding unpushed commits that nothing else
/// accounts for.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct OperatorBranch {
    /// The member's path.
    pub path: String,
    pub branch: String,
    /// How many commits no remote-tracking branch reaches.
    pub unpushed: usize,
    pub commits: Vec<crate::log::LogCommit>,
    /// The branch takes in from a remote branch its remote no longer has.
    pub gone_upstream: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UnreadableProject {
    pub project: ProjectName,
    /// The load error, as the warning on stderr prints it.
    pub error: String,
}

/// The in-flight op recorded at this workspace (owner) or the workspace this
/// one leases to (owner elsewhere), disclosed read-only on `rwv status`.
///
/// The record minus the tip tables (`advanced_tips`/`converged_tips`): those
/// are replay bookkeeping an operator deciding `--continue` vs `abort`
/// doesn't need. `verb` and `phase` are the op-state crate's own types, not a
/// re-encoding of them — a second vocabulary for the same facts is the thing
/// this disclosure must not become.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct OpStatus {
    pub id: String,
    pub verb: op_state::OpVerb,
    pub phase: op_state::OpPhase,
    pub started_at: String,
    /// Workspace holding the full op record (`.rwv-op`) — the workspace
    /// `--continue`/`abort` must run from. Equal to the reporting workspace
    /// unless this workspace only holds a lease.
    pub owner: WirePath,
    pub source: WirePath,
    /// The workspace that has the landing's target checked out; absent where
    /// it is checked out nowhere and the landing moves it as refs.
    pub target: Option<WirePath>,
}

impl From<&op_state::ResolvedOwner> for OpStatus {
    fn from(resolved: &op_state::ResolvedOwner) -> Self {
        Self {
            id: resolved.record.id.to_string(),
            verb: resolved.record.verb,
            phase: resolved.record.phase.clone(),
            started_at: resolved.record.started_at.clone(),
            owner: WirePath::of(resolved.owner_workspace.as_path()),
            source: WirePath::of(resolved.record.source.as_path()),
            target: resolved
                .record
                .target
                .as_ref()
                .map(|target| WirePath::of(target.as_path())),
        }
    }
}

/// Relation between the current branch tip and the lock SHA.
///
/// The two clone-health variants (`Missing` / `Unreachable`) name distinct
/// failure modes, neither of which is `NoLock`:
///
/// - `Missing` — the clone directory is absent from disk entirely (out-of-band
///   `rm -rf`, never fetched, etc.). The lock entry may be fine; the repair
///   verb is a re-clone / `rwv update`.
///
/// - `Unreachable` — the clone directory exists but the SHA pinned in the lock
///   is not present in the local object store (history rewritten, shallow
///   clone, object pruned). The repair is `git fetch` in that clone, which
///   re-materializes the missing object; `rwv update` does not fetch into a
///   clone that is present.
///
/// Neither state should be attributed to the lock file itself — surfacing them
/// as `no-lock` misdirects operators at the wrong repair path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LockRelation {
    Ok,
    Ahead,
    Behind,
    Diverged,
    NoLock,
    Unknown,
    /// Clone directory is absent from disk (out-of-band removal, never fetched).
    /// Repair: re-clone / `rwv update`.
    Missing,
    /// Clone directory exists but the locked SHA is not in the local object
    /// store (history rewritten, shallow clone, object pruned).
    /// Repair: `git fetch` in that clone to materialize the missing object;
    /// `rwv update` does not fetch into a clone that is present.
    Unreachable,
}

impl std::fmt::Display for LockRelation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::wire_token::wire_token(self))
    }
}

/// Recorded-parent exposure for a per-repo status entry.
///
/// Parent identity comes from the workweave's `.rwv-workweave` marker
/// (`parent:`), never from the branch name: a workweave's branch is the flat
/// `<project>--<name>`, which names no parent, and adoption can re-point the
/// parent to primary. Consumers that need the parent must read this field.
///
/// `path` is the recorded parent workspace path (identical for every repo in
/// the workweave). `tip` is this specific repo's parent tip — the SHA that
/// `git rev-parse HEAD` yields in the parent's checkout of the SAME repo — or
/// `None` when the parent has no checkout of this repo (or HEAD is
/// unreadable). The tip is what `git log <parent-tip>..HEAD` needs to compute
/// the workweave's unique commits without re-deriving branch layout.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ParentInfo {
    /// Recorded parent workspace path (from the `.rwv-workweave` marker).
    pub path: WirePath,
    /// This repo's HEAD in the parent's checkout, if resolvable.
    pub tip: Option<CommitId>,
}

/// Per-repo status entry.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RepoStatus {
    pub path: String,
    pub branch: Option<String>,
    pub tip: Option<String>,
    pub lock_sha: Option<String>,
    /// `true` when the lock entry holds a tag or branch name rather than a
    /// commit id; `rwv lock` records the commit id.
    pub lock_pinned_by_name: bool,
    pub relation: LockRelation,
    pub mid_op: Option<MidOperation>,
    pub role: Role,
    pub url: String,
    pub project: ProjectName,
    pub absolute_path: WirePath,
    /// Recorded parent (path + per-repo parent tip) when CWD is a workweave;
    /// `None` in the primary weave (no marker, hence no recorded parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ParentInfo>,
}

/// Classify a repo's HEAD tip against its lock SHA into a [`LockRelation`].
///
/// The single ancestry gate the sync engine relies on. Relations
/// are named from the TIP's vantage (the opposite of "lock behind HEAD" prose):
/// `ahead` means the tip is a strict descendant of the lock (new commits since
/// last lock — the benign in-progress shape), `behind` means the tip is a strict
/// ancestor of the lock (a reset or an `update` without FF), `diverged` means
/// neither is an ancestor of the other. Exposed `pub(crate)` so `sync` can reuse
/// this exact vocabulary rather than inventing a parallel enum.
pub(crate) fn compute_relation(
    vcs: &dyn Vcs,
    repo_abs: &Path,
    tip: &Option<ResolvedRevisionId>,
    lock_sha: &Option<ResolvedRevisionId>,
) -> LockRelation {
    let (tip, lock) = match (tip, lock_sha) {
        (Some(t), Some(l)) => (t, l),
        (_, None) => return LockRelation::NoLock,
        (None, _) => return LockRelation::Unknown,
    };

    if tip == lock {
        return LockRelation::Ok;
    }

    let tip_ahead = vcs.is_ancestor(repo_abs, lock, tip).unwrap_or(false);
    let tip_behind = vcs.is_ancestor(repo_abs, tip, lock).unwrap_or(false);

    match (tip_ahead, tip_behind) {
        (true, _) => LockRelation::Ahead,
        (_, true) => LockRelation::Behind,
        _ => LockRelation::Diverged,
    }
}

/// The name of the ref `repo_abs`'s checkout is on, for the `branch` column.
///
/// Status asks [`Vcs::head_attachment`] only what a checkout is on: it never
/// writes a ref, so it wants no witness.
///
/// What each arm means:
///
/// - `Attached` / `Unborn` — HEAD is symbolic, so a name is what the column
///   is for. Unborn still reports its name: a branch with no commits yet is
///   still the branch the checkout is on, and the empty `tip` column is what
///   says there is nothing on it.
/// - `Detached` — no ref names this checkout; the column renders `-`.
/// - `Err` — `NotARepo` or an unreadable ref database. Still `-`, because
///   the `branch` field is `Option<String>` in the committed JSON schema and
///   this is a report, not a gate: `relation` already carries the health
///   signal (`Missing` / `Unreachable`) that tells an operator which repair
///   verb to reach for. Distinguishing the two in the *output* would be a
///   schema change.
fn checkout_branch(vcs: &dyn Vcs, repo_abs: &Path) -> Option<String> {
    use crate::vcs::HeadAttachment;
    match vcs.head_attachment(repo_abs) {
        Ok(HeadAttachment::Attached(a)) => Some(a.to_string()),
        Ok(HeadAttachment::Unborn(u)) => Some(u.name().as_str().to_owned()),
        Ok(HeadAttachment::Detached(_)) | Err(_) => None,
    }
}

fn project_names_for_ctx(ctx: &WorkspaceContext) -> Vec<ProjectName> {
    match &ctx.checkout {
        Checkout::Primary { project: Some(p) } => vec![p.clone()],
        Checkout::Workweave { project, .. } => vec![project.clone()],
        Checkout::Primary { project: None } => {
            crate::workspace::discover_projects(ctx.active_path())
        }
    }
}

/// Run `rwv status` for the resolved invocation context.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_status(
    ctx: &WorkspaceContext,
    output: OutputMode,
    filter: &RepoFilter,
) -> anyhow::Result<()> {
    // When a specific project is named (via .rwv-active or --project), verify
    // it exists on disk before proceeding. When no project is active, the
    // no-active-project path in project_names_for_ctx falls back to listing
    // all projects — that path is fine and does not need the disk check here.
    if ctx.active_project().is_some() {
        ctx.require_active_project_on_disk()?;
    }

    let workspace_dir = ctx.active_path().to_path_buf();

    // Recorded parent path (workweave-level; identical for every repo).
    // `None` in the primary weave, where there is no marker and hence no
    // recorded parent. Never reconstructed from a branch name (which is
    // wrong for stacked or adopted-to-primary parents).
    let recorded_parent: Option<std::path::PathBuf> = match &ctx.checkout {
        Checkout::Primary { .. } => None,
        Checkout::Workweave { parent, .. } => Some(parent.as_path().to_path_buf()),
    };

    let mut entries: Vec<RepoStatus> = Vec::new();
    let mut unreadable_projects: Vec<UnreadableProject> = Vec::new();
    let mut judged = crate::diagnosis::Report::default();
    let mut entry_changes = String::new();
    let mut records: Vec<crate::branch::BranchRecordStatus> = Vec::new();
    let mut aligned: Vec<AlignedStatus> = Vec::new();
    let mut operator_branches: Vec<OperatorBranch> = Vec::new();
    let accounting = Accounting::scan(ctx.primary_path())?;
    let mut live_pins: Option<
        std::collections::BTreeMap<crate::manifest::RepoPath, Vec<crate::pins::LivePin>>,
    > = None;

    for pname in project_names_for_ctx(ctx) {
        let project_dir = project_dir(&workspace_dir, &pname);
        let project = match Project::from_dir(&project_dir, pname.clone()) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "{}",
                    crate::manifest::unreadable_project_warning(&pname, &e)
                );
                unreadable_projects.push(UnreadableProject {
                    project: pname,
                    error: format!("{e:#}"),
                });
                continue;
            }
        };
        match crate::branch::project_record(&pname, &project_dir) {
            Ok(record) => records.extend(record),
            Err(e) => {
                eprintln!("rwv status: could not read the branch records of project {pname}: {e:#}")
            }
        }
        let aligner = match Aligner::open(&workspace_dir, &pname, accounting.clone()) {
            Ok(aligner) => Some(aligner),
            Err(e) => {
                eprintln!("rwv status: could not read the alignment of project {pname}: {e:#}");
                None
            }
        };
        if let Some(aligner) = aligner.as_ref().filter(|_| filter.is_empty()) {
            aligned.push(aligned_status(&pname, aligner));
            if !output.is_json() {
                let standing = crate::entry_report::standing(
                    crate::vcs::project_vcs().as_ref(),
                    &project_dir,
                    aligner,
                    &project.manifest,
                );
                entry_changes.push_str(&crate::entry_report::render(
                    "status", ctx, aligner, &standing,
                ));
            }
            if let Some(line) = project_line(&pname, &project_dir, aligner) {
                judged.add(line);
            }
            match aligner.record_change_line("status") {
                Ok(line) => judged.extend(line),
                Err(e) => eprintln!(
                    "rwv status: could not compare the branch record of project {pname}: {e:#}"
                ),
            }
        }
        let aligned_lock = project.lock.clone();
        // Resolve lock entries against their on-disk repos so equality with
        // a tip ResolvedRevisionId (which always carries the canonical SHA) works
        // whether the lock pinned a tag, branch, or raw SHA.
        //
        // We keep the raw lock alongside the resolved form so we can:
        //   (a) show the lock version even for repos whose clone is missing,
        //   (b) detect when a lock entry exists but the SHA is unreachable
        //       in the local object store (resolve_versions puts those in
        //       `failures` rather than including them in the resolved map).
        let (lock, lock_resolve_failures) = match project.lock {
            Some(raw) => {
                let raw_clone = raw.clone();
                let (resolved, failures) = raw.resolve_versions(&workspace_dir);
                (Some((raw_clone, resolved)), failures)
            }
            None => (None, Vec::new()),
        };

        for (repo_path, entry) in &project.manifest.repositories {
            if !filter.matches(repo_path, entry.role) {
                continue;
            }
            let vcs = vcs_for(entry.vcs_type);
            let repo_abs = workspace_dir.join(repo_path.as_path());

            if !repo_abs.exists() {
                // Clone directory is absent.  Retrieve the raw lock version
                // for the `lock_sha` display field (the resolved map omits
                // missing repos, but the raw lock still has the entry).
                let raw_lock_entry = lock.as_ref().and_then(|(raw, _)| raw.get_entry(repo_path));
                let raw_lock_sha = raw_lock_entry.map(|e| e.version.as_str().to_owned());
                let lock_pinned_by_name = raw_lock_entry.is_some_and(|e| !e.version.is_commit_id());
                entries.push(RepoStatus {
                    path: repo_path.to_string(),
                    branch: None,
                    tip: None,
                    lock_sha: raw_lock_sha,
                    lock_pinned_by_name,
                    relation: LockRelation::Missing,
                    mid_op: None,
                    role: entry.role,
                    url: entry.url.to_string(),
                    project: pname.clone(),
                    absolute_path: WirePath::of(&repo_abs),
                    parent: recorded_parent.as_ref().map(|parent_path| ParentInfo {
                        path: WirePath::of(parent_path),
                        tip: None,
                    }),
                });
                continue;
            }

            // Clone dir exists — check whether the locked SHA is unreachable
            // in the local object store.  `resolve_versions` puts such entries
            // in `failures`; they are absent from the resolved lock map.
            if lock_resolve_failures.iter().any(|(p, _)| p == repo_path) {
                // The raw lock has an entry for this repo (it was in
                // `failures`) but the SHA cannot be resolved on disk.
                let raw_lock_entry = lock.as_ref().and_then(|(raw, _)| raw.get_entry(repo_path));
                let raw_lock_sha = raw_lock_entry.map(|e| e.version.as_str().to_owned());
                let lock_pinned_by_name = raw_lock_entry.is_some_and(|e| !e.version.is_commit_id());

                let branch = checkout_branch(vcs.as_ref(), &repo_abs);

                let tip = vcs.head_revision(&repo_abs).ok();

                let mid_op = vcs.mid_operation(&repo_abs);

                judged.add(
                    crate::diagnosis::State::PinUnreachable
                        .of(repo_path)
                        .member()
                        .checkout(&repo_abs)
                        .bind("<m>", repo_path.as_str())
                        .subject(
                            raw_lock_sha
                                .as_deref()
                                .map_or("", crate::vcs::short_revision),
                        )
                        .judge(vcs.as_ref()),
                );

                let parent = recorded_parent.as_ref().map(|parent_path| {
                    let parent_repo_abs = parent_path.join(repo_path.as_path());
                    let parent_tip = vcs
                        .head_revision(&parent_repo_abs)
                        .ok()
                        .map(|r| r.id().clone());
                    ParentInfo {
                        path: WirePath::of(parent_path),
                        tip: parent_tip,
                    }
                });

                entries.push(RepoStatus {
                    path: repo_path.to_string(),
                    branch,
                    tip: tip.map(|r| r.display_str().to_owned()),
                    lock_sha: raw_lock_sha,
                    lock_pinned_by_name,
                    relation: LockRelation::Unreachable,
                    mid_op,
                    role: entry.role,
                    url: entry.url.to_string(),
                    project: pname.clone(),
                    absolute_path: WirePath::of(&repo_abs),
                    parent,
                });
                continue;
            }

            if let Some(line) = judge_push_branch(vcs.as_ref(), repo_path, &repo_abs, entry)
                .filter(|line| line.state() != crate::diagnosis::State::PushBranchAhead)
            {
                judged.add(line);
            }
            let set_back = matches!(vcs.set_back_from(&repo_abs), Ok(Some(_)));
            let mid_operation = vcs.mid_operation(&repo_abs).is_some();
            if mid_operation {
                judged.extend(crate::diagnosis::judge_edits(
                    vcs.as_ref(),
                    repo_path,
                    &repo_abs,
                    Vec::new(),
                    None,
                ));
            } else if set_back {
                if let Some(line) = crate::diagnosis::judge_edits(
                    vcs.as_ref(),
                    repo_path,
                    &repo_abs,
                    Vec::new(),
                    None,
                ) {
                    judged.add(line);
                }
            } else if let Some(aligner) = &aligner {
                let member = Member {
                    path: repo_path,
                    entry,
                    dir: &repo_abs,
                    vcs: vcs.as_ref(),
                };
                match aligner.status_lines(&member, aligned_lock.as_ref(), repo_path) {
                    Ok(lines) => lines.into_iter().for_each(|line| judged.add(line)),
                    Err(e) => {
                        eprintln!("rwv status: {repo_path}: could not judge its alignment: {e:#}")
                    }
                }
            }
            match operator_branches_of(vcs.as_ref(), repo_path, &repo_abs, &accounting) {
                Ok(found) => operator_branches.extend(found),
                Err(e) => eprintln!("rwv status: {repo_path}: could not list its branches: {e:#}"),
            }

            let branch = checkout_branch(vcs.as_ref(), &repo_abs);

            let tip = vcs.head_revision(&repo_abs).ok();

            let lock_sha = lock
                .as_ref()
                .and_then(|(_, resolved)| resolved.get_entry(repo_path))
                .map(|e| e.version.clone());

            let relation = compute_relation(vcs.as_ref(), &repo_abs, &tip, &lock_sha);
            if let Some(head) = tip.as_ref().filter(|_| !set_back && !mid_operation) {
                let judging = match &lock_sha {
                    None => Some(
                        crate::diagnosis::State::PinNone
                            .of(repo_path)
                            .member()
                            .checkout(&repo_abs)
                            .bind("<m>", repo_path.as_str())
                            .judge(vcs.as_ref()),
                    ),
                    Some(pin) => {
                        let live = live_pins.get_or_insert_with(|| {
                            crate::pins::live_pins(ctx.primary_path()).unwrap_or_default()
                        });
                        let held = live.get(repo_path).map(Vec::as_slice).unwrap_or_default();
                        match crate::pins::standing(vcs.as_ref(), &repo_abs, head, pin, held) {
                            Ok(standing) => {
                                let reset = matches!(standing, crate::pins::Standing::Diverged)
                                    .then(|| {
                                        crate::feature_sync::reset_offer(ctx, &pname, repo_path)
                                    })
                                    .flatten();
                                crate::pins::judge(
                                    repo_path,
                                    &repo_abs,
                                    vcs.as_ref(),
                                    &standing,
                                    (head, pin),
                                    reset.as_ref(),
                                )
                            }
                            Err(e) => {
                                eprintln!(
                                    "rwv status: {repo_path}: could not judge its pin: {e:#}"
                                );
                                None
                            }
                        }
                    }
                };
                judged.extend(judging);
            }

            let mid_op = vcs.mid_operation(&repo_abs);

            // Per-repo parent tip: resolve THIS repo's HEAD in the parent's
            // checkout of the same repo path. Read from the recorded parent
            // path, not a reconstructed branch name.
            let parent = recorded_parent.as_ref().map(|parent_path| {
                let parent_repo_abs = parent_path.join(repo_path.as_path());
                let parent_tip = vcs
                    .head_revision(&parent_repo_abs)
                    .ok()
                    .map(|r| r.id().clone());
                ParentInfo {
                    path: WirePath::of(parent_path),
                    tip: parent_tip,
                }
            });

            entries.push(RepoStatus {
                path: repo_path.to_string(),
                branch,
                tip: tip.map(|r| r.display_str().to_owned()),
                lock_sha: lock_sha.map(|r| r.display_str().to_owned()),
                lock_pinned_by_name: lock
                    .as_ref()
                    .and_then(|(raw, _)| raw.get_entry(repo_path))
                    .is_some_and(|e| !e.version.is_commit_id()),
                relation,
                mid_op,
                role: entry.role,
                url: entry.url.to_string(),
                project: pname.clone(),
                absolute_path: WirePath::of(&repo_abs),
                parent,
            });
        }
    }

    let (op, unreadable_op) = match op_state::resolve_to_owner(&workspace_dir) {
        Ok(op) => (op, None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    if op.is_some() {
        judged = judged.without_choices();
    }
    let backups = match ctx.active_project() {
        Some(active) if !unreadable_projects.iter().any(|u| &u.project == active) => {
            crate::backup::held_by_project(ctx)?
        }
        _ => Vec::new(),
    };

    if output.is_json() {
        let envelope = StatusJsonOutput {
            schema_url: STATUS_SCHEMA_URL.to_string(),
            repos: entries,
            unreadable_projects,
            records,
            aligned,
            judged: judged.output(),
            operator_branches,
            backups,
            resolution: Some(ctx.resolution()),
            op: op.as_ref().map(OpStatus::from),
            unreadable_op,
        };
        let out = serde_json::to_string_pretty(&envelope)
            .context("failed to serialize status to JSON")?;
        outln!("{out}");
    } else {
        if let Checkout::Workweave { name, .. } = &ctx.checkout {
            if name.recorded().is_none() {
                eprintln!("{}", crate::workspace::UNREGISTERED_WORKWEAVE_NOTICE);
            }
        }
        if let Some(op) = &op {
            print_op_header(op);
        }
        if let Some(why) = &unreadable_op {
            outln!("{why}");
        }
        print_table(&entries);
        let mut lines: Vec<String> = records.iter().map(|record| record.line()).collect();
        lines.extend(aligned.iter().map(AlignedStatus::line));
        lines.extend(operator_branches.iter().map(OperatorBranch::line));
        for held in &backups {
            for backup in &held.backups {
                lines.push(format!(
                    "backup {}  {}  {}",
                    held.path,
                    backup.name,
                    backup.tip.short()
                ));
            }
        }
        if !lines.is_empty() {
            outln!();
            for line in lines {
                outln!("{line}");
            }
        }
        if !entry_changes.is_empty() {
            outln!();
            outln!("{}", entry_changes.trim_end_matches('\n'));
        }
        if !judged.is_empty() {
            outln!();
            outln!("{}", judged.render("status").trim_end_matches('\n'));
        }
    }

    Ok(())
}

impl AlignedStatus {
    fn line(&self) -> String {
        let project = crate::workspace::project_rel_path(&self.project);
        let line = match (&self.branch, self.detached) {
            (Some(branch), _) => format!("{project} aligned for {branch}"),
            (None, true) => format!("{project} aligned detached"),
            (None, false) => format!("{project} aligned for no branch"),
        };
        match &self.unreadable {
            Some(why) => format!("{line} (workspace state reads as absent: {why})"),
            None => line,
        }
    }
}

impl OperatorBranch {
    fn line(&self) -> String {
        let gone = if self.gone_upstream {
            " (upstream gone)"
        } else {
            ""
        };
        format!(
            "operator branch {}  {}  {} unpushed{gone}",
            self.path, self.branch, self.unpushed
        )
    }
}

fn aligned_status(project: &ProjectName, aligner: &Aligner) -> AlignedStatus {
    use crate::alignment::Aligned;
    let (branch, detached) = match &aligner.state.aligned {
        Some(Aligned::Branch(name)) => (Some(name.clone()), false),
        Some(Aligned::Detached) => (None, true),
        None => (None, false),
    };
    AlignedStatus {
        project: project.clone(),
        branch,
        detached,
        unreadable: aligner.corrupt.clone(),
    }
}

/// The line for the project repo against the branch its workspace last
/// aligned for.
fn project_line(
    project: &ProjectName,
    project_dir: &Path,
    aligner: &Aligner,
) -> Option<crate::diagnosis::Judged> {
    use crate::diagnosis::State;
    let vcs = crate::vcs::project_vcs();
    let judging = match &aligner.standing {
        ProjectStanding::Aligned => return None,
        ProjectStanding::Moved { aligned } => State::AlignmentProjectMoved
            .of(crate::workspace::project_rel_path(project))
            .subject(aligned),
        ProjectStanding::Unaligned => {
            State::AlignmentProjectUnaligned.of(crate::workspace::project_rel_path(project))
        }
        ProjectStanding::Detached => aligner.project_detached().ok()?,
    };
    let aligned = aligner.state_aligned_text();
    let judging = judging.project_repo().checkout(project_dir);
    Some(
        if aligned.is_empty() {
            judging
        } else {
            judging.bind("<b>", aligned)
        }
        .judge(vcs.as_ref()),
    )
}

/// The member's local branches holding unpushed commits that nothing else
/// accounts for.
fn operator_branches_of(
    vcs: &dyn Vcs,
    repo_path: &crate::manifest::RepoPath,
    repo_abs: &Path,
    accounting: &Accounting,
) -> anyhow::Result<Vec<OperatorBranch>> {
    let mut found = Vec::new();
    for name in vcs.list_local_branch_names(repo_abs)? {
        let text = name.as_str();
        if accounting.is_recorded(text) || accounting.declares(repo_path, text) {
            continue;
        }
        if !vcs.checkouts_of_branch(repo_abs, &name)?.is_empty() {
            continue;
        }
        let unpushed = vcs.unpushed_commits(repo_abs, &name)?;
        if unpushed.is_empty() {
            continue;
        }
        let gone_upstream = match vcs.upstream_of(repo_abs, &name)? {
            Some(upstream) => !vcs
                .remote_copies(repo_abs, &upstream.branch)?
                .iter()
                .any(|copy| copy.remote == upstream.remote),
            None => false,
        };
        found.push(OperatorBranch {
            path: repo_path.to_string(),
            branch: text.to_owned(),
            unpushed: unpushed.len(),
            commits: unpushed.into_iter().map(Into::into).collect(),
            gone_upstream,
        });
    }
    Ok(found)
}

/// Where the branch `repo_abs` is on stands to the branch it publishes to,
/// when that is worth a line: judged read-only, on refs as last fetched.
fn judge_push_branch(
    vcs: &dyn Vcs,
    repo_path: &crate::manifest::RepoPath,
    repo_abs: &Path,
    entry: &crate::manifest::RepoEntry,
) -> Option<crate::diagnosis::Judged> {
    use crate::vcs::HeadAttachment;
    let Ok(HeadAttachment::Attached(attached)) = vcs.head_attachment(repo_abs) else {
        return None;
    };
    let url_remote = crate::url_remote::present(vcs, repo_abs, &entry.url).ok()?;
    let (standing, _) = crate::push_branch::standing(vcs, &attached, url_remote.as_ref()).ok()?;
    crate::push_branch::judge(
        vcs,
        &crate::push_branch::Subject {
            repo: repo_path,
            checkout: repo_abs,
            branch: &attached.to_string(),
            declared: Some(&entry.branch),
            member: true,
            republish: false,
            why: None,
        },
        &standing,
    )
}

/// The header `rwv status` prints ahead of the table when this workspace owns
/// or leases an in-flight op: the same fact the in-flight refusal names
/// (verb, phase, age, owner dir, op id, both remedies), disclosed without an
/// operator having to attempt a mutation first to see it.
fn print_op_header(op: &op_state::ResolvedOwner) {
    outln!(
        "{verb} in progress (op {id}, mid `{phase}`, started {elapsed} ago) at {owner}",
        verb = op.record.verb,
        id = op.record.id,
        phase = op.record.phase,
        elapsed = op_state::elapsed_since(&op.record.started_at),
        owner = op.owner_workspace.as_path().display(),
    );
    outln!(
        "  resume with `{resume}`; or {abort}",
        resume = op_state::resume_command_at(op.record.verb, op.owner_workspace.as_path()),
        abort = op_state::abort_rolls_back_at(op.owner_workspace.as_path()),
    );
}

/// A revision column's cell: a commit id abbreviated, a name in full.
fn shown_revision(revision: &Option<String>) -> String {
    match revision {
        None => "-".to_owned(),
        Some(s) => crate::vcs::short_revision(s).to_owned(),
    }
}

fn print_table(entries: &[RepoStatus]) {
    // Measure column widths.
    let path_w = entries
        .iter()
        .map(|e| e.path.len())
        .max()
        .unwrap_or(0)
        .max(4);
    let branch_w = entries
        .iter()
        .map(|e| e.branch.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0)
        .max(6);
    let tip_w = entries
        .iter()
        .map(|e| shown_revision(&e.tip).len())
        .max()
        .unwrap_or(0)
        .max(3);
    let lock_w = entries
        .iter()
        .map(|e| shown_revision(&e.lock_sha).len())
        .max()
        .unwrap_or(0)
        .max(4);

    for entry in entries {
        let branch = entry.branch.as_deref().unwrap_or("-");
        let tip = shown_revision(&entry.tip);
        let lock = shown_revision(&entry.lock_sha);
        let mid = entry
            .mid_op
            .map(|op| format!("  [{op}]"))
            .unwrap_or_default();
        let by_name = if entry.lock_pinned_by_name {
            "  [pinned by name; run `rwv lock`]"
        } else {
            ""
        };

        outln!(
            "{:<path_w$}  {:<branch_w$}  {:<tip_w$}  lock: {:<lock_w$}  [{}]{}{}",
            entry.path,
            branch,
            tip,
            lock,
            entry.relation,
            mid,
            by_name,
            path_w = path_w,
            branch_w = branch_w,
            tip_w = tip_w,
            lock_w = lock_w,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip the envelope through serde so the `$schema` rename and the
    /// `repos` array shape are pinned independent of the live status path.
    #[test]
    fn status_json_envelope_round_trips() {
        let envelope = StatusJsonOutput {
            schema_url: STATUS_SCHEMA_URL.to_string(),
            unreadable_projects: Vec::new(),
            records: Vec::new(),
            aligned: Vec::new(),
            judged: Vec::new(),
            operator_branches: Vec::new(),
            backups: Vec::new(),
            repos: vec![RepoStatus {
                path: "github/org/repo".into(),
                branch: Some("main".into()),
                tip: Some("abc123".into()),
                lock_sha: Some("abc123".into()),
                lock_pinned_by_name: false,
                relation: LockRelation::Ok,
                mid_op: None,
                role: Role::Owned,
                url: "https://example.com/repo.git".into(),
                project: ProjectName::new("demo").unwrap(),
                absolute_path: crate::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/github/org/repo",
                )),
                parent: None,
            }],
            resolution: None,
            op: None,
            unreadable_op: None,
        };

        let json = serde_json::to_string(&envelope).expect("serializes");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parses");

        // Wire-shape: top-level $schema + repos array.
        assert_eq!(
            v["$schema"],
            serde_json::Value::String(STATUS_SCHEMA_URL.to_string())
        );
        let repos = v["repos"].as_array().expect("repos is an array");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["path"], "github/org/repo");
        assert_eq!(repos[0]["relation"], "ok");

        // Typed round-trip back to the envelope struct.
        let decoded: StatusJsonOutput = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded.schema_url, STATUS_SCHEMA_URL);
        assert_eq!(decoded.repos.len(), 1);
        assert_eq!(decoded.repos[0].path, "github/org/repo");
        assert_eq!(decoded.repos[0].relation, LockRelation::Ok);
        // A repo with no recorded parent omits the field entirely.
        assert!(
            repos[0].get("parent").is_none(),
            "parent field should be omitted when None; got: {}",
            repos[0]
        );
        assert!(decoded.repos[0].parent.is_none());
        // No in-flight op: the top-level `op` key is omitted, not null — a
        // consumer branches on key presence the same way it does for `parent`.
        assert!(
            v.get("op").is_none(),
            "op field should be omitted when None; got: {v}"
        );
        assert!(decoded.op.is_none());
    }

    /// An in-flight op discloses the record minus the tip tables, keyed by
    /// the op-state crate's own `verb`/`phase` types.
    #[test]
    fn status_json_op_field_round_trips() {
        let envelope = StatusJsonOutput {
            schema_url: STATUS_SCHEMA_URL.to_string(),
            repos: vec![],
            unreadable_projects: Vec::new(),
            records: Vec::new(),
            aligned: Vec::new(),
            judged: Vec::new(),
            operator_branches: Vec::new(),
            backups: Vec::new(),
            resolution: None,
            op: Some(OpStatus {
                id: "1779769917405921588".into(),
                verb: op_state::OpVerb::Sync,
                phase: op_state::OpPhase::Replay,
                started_at: "2026-06-10T21:14:03Z".into(),
                owner: crate::path_spelling::WirePath::of(std::path::Path::new("/abs/ws/target")),
                source: crate::path_spelling::WirePath::of(std::path::Path::new("/abs/ws/source")),
                target: Some(crate::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/ws/target",
                ))),
            }),
            unreadable_op: None,
        };

        let json = serde_json::to_string(&envelope).expect("serializes");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parses");
        assert_eq!(v["op"]["id"], "1779769917405921588");
        assert_eq!(v["op"]["verb"], "sync");
        assert_eq!(v["op"]["phase"], "replay");
        assert_eq!(v["op"]["owner"], "/abs/ws/target");
        assert_eq!(v["op"]["source"], "/abs/ws/source");
        assert!(v["op"].get("overrides").is_none());
        // The record minus the tip tables: neither key exists on the wire.
        assert!(v["op"].get("advanced_tips").is_none());
        assert!(v["op"].get("converged_tips").is_none());

        let decoded: StatusJsonOutput = serde_json::from_str(&json).expect("deserializes");
        let op = decoded.op.expect("op present after round-trip");
        assert_eq!(op.verb, op_state::OpVerb::Sync);
        assert_eq!(op.phase, op_state::OpPhase::Replay);
    }

    /// The `parent` field carries both the recorded path and the per-repo
    /// parent tip, and round-trips cleanly.
    #[test]
    fn status_json_parent_field_round_trips() {
        let envelope = StatusJsonOutput {
            schema_url: STATUS_SCHEMA_URL.to_string(),
            unreadable_projects: Vec::new(),
            records: Vec::new(),
            aligned: Vec::new(),
            judged: Vec::new(),
            operator_branches: Vec::new(),
            backups: Vec::new(),
            repos: vec![RepoStatus {
                path: "github/org/repo".into(),
                branch: Some("app--wwb/app--wwa/main".into()),
                tip: Some("deadbeef".into()),
                lock_sha: Some("deadbeef".into()),
                lock_pinned_by_name: false,
                relation: LockRelation::Ok,
                mid_op: None,
                role: Role::Owned,
                url: "https://example.com/repo.git".into(),
                project: ProjectName::new("demo").unwrap(),
                absolute_path: crate::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/github/org/repo",
                )),
                parent: Some(ParentInfo {
                    // A STACKED parent path: the recorded parent is itself a
                    // workweave, NOT primary. basename(parent)/main would be
                    // wrong here — the field pins the real path instead.
                    path: crate::path_spelling::WirePath::of(std::path::Path::new(
                        "/abs/.workweaves/demo--wwa",
                    )),
                    tip: Some(CommitId::parse("cafef00d00000000000000000000000000000000").unwrap()),
                }),
            }],
            resolution: None,
            op: None,
            unreadable_op: None,
        };

        let json = serde_json::to_string(&envelope).expect("serializes");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parses");
        assert_eq!(
            v["repos"][0]["parent"]["path"],
            "/abs/.workweaves/demo--wwa"
        );
        assert_eq!(
            v["repos"][0]["parent"]["tip"],
            "cafef00d00000000000000000000000000000000"
        );

        let decoded: StatusJsonOutput = serde_json::from_str(&json).expect("deserializes");
        let parent = decoded.repos[0]
            .parent
            .as_ref()
            .expect("parent present after round-trip");
        assert_eq!(parent.path.as_str(), "/abs/.workweaves/demo--wwa");
        assert_eq!(
            parent.tip.as_ref().map(CommitId::as_str),
            Some("cafef00d00000000000000000000000000000000")
        );
    }
}
