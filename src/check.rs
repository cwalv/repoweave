//! Convention checks: orphaned clones, dangling refs, stale locks, index drift, working-tree drift, etc.
//!
//! `rwv doctor` builds a workspace-wide inventory from all projects, then runs
//! a series of checks. Integration check hooks are run separately.

use crate::git::git_command;
use crate::integration::Issue;
use crate::manifest::{Project, ProjectName, RepoPath, Role, WorkweaveName};
use crate::vcs::ResolvedRevisionId;
use anyhow::Context;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The kinds of convention violations `rwv doctor` can find.
///
/// Each variant carries enough data to produce a useful message.
/// Separating the description (this enum) from execution (the checker)
/// makes results testable without touching the filesystem.
#[derive(Debug)]
pub enum CheckViolation {
    /// A directory under a registry path not listed in any project's `rwv.yaml`.
    OrphanedClone { path: RepoPath },

    /// An `rwv.yaml` entry pointing to a path not present on disk.
    DanglingReference {
        project: ProjectName,
        repo: RepoPath,
    },

    /// An `rwv.yaml` entry missing the `role` field.
    MissingRole {
        project: ProjectName,
        repo: RepoPath,
    },

    /// A project's `rwv.lock` doesn't match current HEAD SHAs.
    StaleLock {
        project: ProjectName,
        repo: RepoPath,
        locked: ResolvedRevisionId,
        actual: ResolvedRevisionId,
    },

    /// A worktree missing from a workweave, or an extra worktree not in the manifest.
    WorkweaveDrift {
        workweave: WorkweaveName,
        kind: DriftKind,
        repo: RepoPath,
    },

    /// A git repo's index does not match its HEAD tree (silent stale-index from
    /// shared-ref advance in a sibling worktree).
    IndexDrift {
        /// Workweave name; `None` for repos in the primary weave.
        workweave: Option<WorkweaveName>,
        repo: RepoPath,
        kind: IndexDriftKind,
    },

    /// A git repo's working-tree files do not match its HEAD tree (stale on-disk
    /// content after shared-ref advance in a sibling worktree).
    WorkingTreeDrift {
        workweave: Option<WorkweaveName>,
        repo: RepoPath,
        kind: WorkingTreeDriftKind,
    },

    /// A project repo is missing the `rwv.lock merge=rwv-ours` entry in
    /// `.gitattributes`. Without it, `rwv sync`'s native rebase would carry
    /// user lock-edits through the merge inputs instead of letting Phase 3
    /// regenerate them. Auto-fixable: append the line, or (if the legacy
    /// `merge=ours` spelling is present in `.gitattributes`) migrate it in
    /// place and commit. The committed-form check is the one sync's
    /// invariant reads; the migration commit is what makes it visible on
    /// the next rebase.
    MissingReplayExclusion { project: ProjectName },

    /// A project's `rwv.yaml` uses the legacy `role: primary` spelling
    /// (replaced by `role: owned`; the back-compat alias has since been
    /// dropped). Auto-fixable: rewrite each affected line in place,
    /// preserving comments and key order.
    LegacyRolePrimary {
        /// Project the manifest belongs to (or a synthetic name when the
        /// detector runs without a fully-loaded project — manifests with
        /// `role: primary` can't reach `Project::from_dir` since the
        /// parse fails).
        project: ProjectName,
        /// Absolute path to the offending `rwv.yaml`.
        manifest_path: PathBuf,
    },

    /// `.rwv-active` names a project whose `projects/<name>/` directory does
    /// not exist on disk. Any action verb that reads the active project will
    /// fail with a confusing downstream error. Auto-fixable: clear
    /// `.rwv-active` (or prompt to pick from existing projects under
    /// `--fix`).
    DanglingActiveProject {
        /// The project name recorded in `.rwv-active`.
        project: ProjectName,
        /// The `projects/` directory that does not exist on disk.
        missing_dir: PathBuf,
    },

    /// A `.rwv-workweave` marker file is missing the required `parent:` field
    /// (written before parent tracking landed). Auto-fixable: append
    /// `parent: <primary value>` to the file on disk.
    LegacyWorkweaveMarker {
        /// Absolute path to the offending `.rwv-workweave` file.
        marker_path: PathBuf,
        /// The `primary:` value from the marker, used as the backfill value.
        primary: PathBuf,
    },

    /// A project's `rwv.yaml` exists but cannot be parsed.
    ///
    /// Reported as an `Error`-severity violation so the operator is not
    /// left with zero violations (i.e. an apparent "clean" result) for a
    /// project whose manifest is broken. `--fix` does NOT auto-repair this
    /// — the operator must fix the YAML by hand and re-run `rwv doctor`.
    UnparseableProject {
        /// Relative project path (e.g. `my-app`, `org/repo`).
        project: ProjectName,
        /// Absolute path to the offending `rwv.yaml`.
        manifest_path: PathBuf,
        /// Free-form display string of the parse error (from `anyhow::Error::to_string`).
        /// No structured parse-error type is available at this boundary.
        message: String,
    },

    /// A `.rwv-workweave` marker tree anomaly: dangling parent, chain anomaly,
    /// unregistered directory, or foreign-primary marker.
    ///
    /// The `dangling-parent` sub-kind is auto-fixable (`rwv doctor --fix`
    /// re-points the child's `parent:` to primary, which always exists). The
    /// other three sub-kinds are report-only: no auto-fix is safe without
    /// operator input.
    WorkweaveTreeIntegrity {
        /// Absolute path to the workweave directory (or the marker file for
        /// file-level violations).
        workweave_dir: PathBuf,
        /// Discriminator for the specific anomaly detected.
        sub_kind: WorkweaveTreeIntegrityKind,
    },

    /// A provenance violation: a clone's remote URL diverges from the manifest
    /// URL (`origin-url-mismatch`) or a lock-file SHA is absent from the
    /// local object store (`lock-sha-unreachable`).
    ///
    /// Always report-only (no `--fix` path): the `origin-url-mismatch` case
    /// requires the operator to decide whether the manifest or the remote is
    /// the source of truth; reference-role repos may intentionally diverge.
    /// The `lock-sha-unreachable` case requires a fetch from the remote, not
    /// a sync.
    Provenance {
        /// The project the affected repo belongs to.
        project: ProjectName,
        /// Manifest-relative path to the affected repo.
        repo: RepoPath,
        /// Discriminator for the specific provenance anomaly.
        sub_kind: ProvenanceKind,
    },

    /// A clone-topology violation: one of the manifest repos is on disk in a
    /// shape that breaks tier-0 invariants from
    /// [`docs/explanation/joints/clone-topology.md`].
    ///
    /// All four sub-kinds are silent for every higher-tier `rwv doctor`
    /// check (those operate on revisions and content; this one operates on
    /// the physical object-store topology). Always report-only: repair is
    /// an object-store migration (re-parenting), out of `--fix` scope per
    /// the alpha guideline.
    CloneTopology {
        /// Absolute path of the workspace that exhibits the violation. For
        /// `WeaveCloneIsWorktree` and `DisconnectedWeaveClone`, this is the
        /// canonical slot `<weave>/<repo_path>`. For `StandaloneInWorkweave`
        /// and `WrongParentWorktree`, this is the offending workweave
        /// checkout `<workweave>/<repo_path>`.
        workspace_path: PathBuf,
        /// Manifest-relative repo path involved in the violation.
        repo: RepoPath,
        /// Discriminator for the specific topology violation.
        sub_kind: CloneTopologyKind,
    },
    /// Branch-discipline violations enforcing the I3 invariant from the
    /// `clone-topology` joint: every workweave repo checkout sits on a
    /// `<project>--<workweave>/<segment>` ephemeral branch, every canonical
    /// clone sits on a non-ephemeral branch, and stale ephemeral branches
    /// left over from deleted workweaves are surfaced (and, for the safe
    /// class only, removable via `--fix`).
    ///
    /// The check catches manual operations the clone-topology scan cannot
    /// see — e.g. `git switch main` inside a workweave, or a `branch -D`
    /// that left behind an `<project>--<dead>/main` branch in the canonical.
    ///
    /// See `docs/explanation/joints/clone-topology.md` (I3) and
    /// `docs/explanation/joints/shared-refs-drift.md` (safe/live doctrine).
    BranchDiscipline {
        /// Absolute path to the repo checkout (workweave repo for (a),
        /// canonical clone for (b) and (c)) where the violation was found.
        repo_path: PathBuf,
        /// Discriminator for the specific branch-discipline anomaly.
        sub_kind: BranchDisciplineKind,
    },
    /// A worktree registration recorded in a repo whose on-disk directory
    /// no longer exists. The administrative entry is stale; auto-fixable
    /// by running `worktree prune` — information-preserving by
    /// construction (the only state being dropped is a pointer to a
    /// directory that already is not there).
    StaleWorktreeRegistration {
        /// Workweave name when the *registering* repo (the one that holds
        /// the stale `.git/worktrees/` entry) lives inside a workweave;
        /// `None` when the registering repo is in the primary weave.
        workweave: Option<WorkweaveName>,
        /// The registering repo's manifest-relative path.
        repo: RepoPath,
        /// Absolute path of the missing worktree directory, as recorded
        /// in the VCS's worktree list.
        missing_path: PathBuf,
    },

    /// A `.rwv-op` file is present at a workspace root. Reports the file's
    /// age and the path so the operator can inspect, resume
    /// (`rwv sync --continue`), or roll back (`rwv abort`). **Never
    /// auto-fixed**: another terminal may be mid-conflict-resolution; rwv
    /// has no daemon to know which workspace the op-state legitimately
    /// belongs to.
    StaleOpState {
        /// Absolute path to the workspace dir that holds the `.rwv-op` file.
        workspace_dir: PathBuf,
        /// Raw `started_at` string from the op-state file (RFC3339 UTC),
        /// preserved verbatim so the operator sees the same value
        /// `op_state::read_owner` would.
        started_at: String,
    },

    /// A `.rwv-op-lease` file whose recorded owner workspace has no matching
    /// `.rwv-op` with the same op id — the **structural dead-lease** case.
    ///
    /// Unlike [`StaleOpState`], this **is** auto-fixable: the classification
    /// is by *structural comparison* — the lease pointer resolves to no
    /// paired owner record (either because the owner file is gone or because
    /// it now belongs to a different op id). No wall-clock input; no timeout;
    /// no daemon-required liveness guess. Dropping a lease whose owner is
    /// provably absent unblocks the workspace without any risk of clobbering
    /// an in-flight op (there is no such op to clobber). See
    /// [`crate::op_state::detect_dead_lease`] for the classification.
    ///
    /// Reports the age of the lease as informational context (RFC3339 +
    /// humanized) so the operator can gauge how long the shared-state
    /// bookkeeping has been off — never as a decision input.
    DeadOpLease {
        /// Absolute path to the workspace dir that holds the dangling lease.
        workspace_dir: PathBuf,
        /// Op id recorded in the lease.
        op_id: String,
        /// Owner workspace the lease pointed at.
        recorded_owner: PathBuf,
        /// Discriminator for the specific dead-lease shape.
        sub_kind: DeadOpLeaseKind,
        /// RFC3339 UTC timestamp at which the lease was written (from the
        /// lease file's `created_at` field). `None` for old lease files
        /// written before this field was added. Observability-only.
        created_at: Option<String>,
    },

    /// A `refs/rwv/pre-op/<op-id>` savepoint whose op-id is not present
    /// in any `.rwv-op` file in this workspace tree. Sub-kind picks the
    /// classification — savepoint tip reachable from current HEAD
    /// (redundant, safely droppable) vs unreachable (the savepoint is
    /// the last pointer to discarded work; keep).
    OrphanedSavepoint {
        /// Workweave name when the holding repo is inside a workweave.
        workweave: Option<WorkweaveName>,
        /// The holding repo's manifest-relative path.
        repo: RepoPath,
        /// Opaque op-id captured from the savepoint ref's trailing path
        /// component (`refs/rwv/pre-op/<op_id>`).
        op_id: String,
        /// Safe-vs-live classification.
        sub_kind: OrphanedSavepointKind,
    },

    /// Version skew across cargo workspace members: the same crate name is
    /// required at different version-req strings by two or more members
    /// (post `workspace = true` indirection). Always **warning** severity
    /// and report-only — the observatory is informational; rwv cannot
    /// mandate versions across sovereign repos. See Finding 3 of
    /// `docs/repoweave/grok-build-export-findings.md`.
    CargoVersionSkew {
        /// The registry crate name (e.g. `serde`, `tokio`).
        crate_name: String,
        /// Per-member requirement strings, sorted for stable output.
        occurrences: Vec<crate::integrations::cargo_workspace::CargoSkewOccurrence>,
    },

    /// A member's `.cargo/config.toml` declares a `[patch.<registry>].<crate>`
    /// key that would silently defeat a weave-level entry for the same key
    /// (cargo's closest-config-wins per-key shadowing — probe P5b in the
    /// design doc). Warning severity, report-only. Doubles as the mandatory
    /// precheck for derived-patch generation: cargo's mismatch diagnostic
    /// actively misleads (blames crates.io) when a patch silently doesn't
    /// apply (probe P6), so surfacing the shadow at scan time preserves the
    /// operator's ability to diagnose the actual cause.
    CargoPatchShadowing {
        /// Weave-level file that carries the (would-be) inert patch entry.
        weave_config: PathBuf,
        /// Member-level `.cargo/config.toml` that wins (closest-wins per key).
        member_config: PathBuf,
        /// Registry sub-table name (typically `crates-io`; git-source
        /// patches use the git URL).
        registry: String,
        /// The specific crate name whose key collides.
        crate_name: String,
    },

    /// A workweave worktree whose canonical clone (the primary-weave clone it
    /// was linked from via `git worktree add`) no longer exists on disk.
    ///
    /// When the canonical clone directory is removed out-of-band, git commands
    /// in the dependent worktree fail silently or with opaque errors — in
    /// particular, `git diff-index HEAD` fails and the former catch-all arm in
    /// `classify_working_tree_drift` would misattribute the failure as
    /// `LiveEdits`. This variant surfaces the true root cause instead.
    ///
    /// Repair: `rwv fetch` (in-place, no SOURCE) re-materializes the canonical
    /// (same as [`DanglingReference`]), then re-run `rwv doctor` to verify.
    /// No auto-fix — doctor never clones (network stays behind explicit verbs).
    MissingCanonicalClone {
        /// Workweave name (always `Some`; this finding only fires for
        /// workweave worktrees, never for primary-weave repos).
        workweave: WorkweaveName,
        /// Manifest-relative path to the affected repo.
        repo: RepoPath,
        /// Absolute path of the canonical clone directory that is missing.
        canonical_path: PathBuf,
    },

    /// A worktree inside a workweave has a `.gitmodules` file but one or more
    /// of its listed submodule paths are empty directories (or absent),
    /// indicating that `git submodule update --init` has never run there.
    ///
    /// Warning severity; report-only. The fix is a single git command named in
    /// the finding message. No network is required for detection — the scanner
    /// only stats the paths listed in `.gitmodules`.
    ///
    /// Emitted by `rwv doctor` when scanning workweave repos. `create_workweave`
    /// attempts the init automatically and emits this state as a warning when
    /// the init fails (e.g., network unreachable at create time).
    UninitializedSubmodule {
        /// Workweave name.
        workweave: WorkweaveName,
        /// Manifest-relative path to the repo that has uninitialized submodules.
        repo: RepoPath,
        /// Submodule paths (relative to the repo root) that are empty on disk.
        empty_paths: Vec<String>,
    },
}

/// Classification of an orphaned savepoint, controlling `--fix` policy.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum OrphanedSavepointKind {
    /// The savepoint tip is reachable from the current branch tip, so
    /// the ref is redundant — the underlying commits are still anchored
    /// by the live branch and dropping the savepoint loses no objects.
    /// `--fix` may drop redundant savepoints.
    Redundant,
    /// The savepoint tip is **not** reachable from the current branch
    /// tip. The ref is the last pointer to commits that would otherwise
    /// become unreachable. `--fix` must not drop these — the reflog is
    /// on the FORBIDDEN tripwire list, same rationale: don't cut the
    /// last recovery path.
    Live,
}

/// Discriminator for [`CheckViolation::DeadOpLease`] findings. Both shapes
/// share the same `--fix` disposition (safe to remove the lease file) but
/// name distinct root causes so the human-facing message can be specific.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum DeadOpLeaseKind {
    /// The recorded owner workspace has no `.rwv-op` file at all — either
    /// the owner workspace was deleted, or the owner record was
    /// hand-removed while the lease survived. The classical
    /// crash-between-acquire-and-mark shape.
    OwnerRecordAbsent,
    /// The recorded owner workspace has an `.rwv-op` file, but with a
    /// *different* op id than the lease references. The owner cleared and
    /// a new op started while this stale lease survived — the lease
    /// points at a completed op, not an in-flight one.
    OwnerOpIdMismatch {
        /// Op id of the record currently living at the owner workspace.
        owner_op_id: String,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DriftKind {
    /// Manifest lists it, but no worktree exists.
    Missing,
    /// Worktree exists, but manifest doesn't list it.
    Extra,
}

/// How a stale index should be treated.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum IndexDriftKind {
    /// Index tree matches the tree of some recent ancestor commit. Safe to
    /// auto-fix with `git reset` — the displaced tree is permanently in the DAG.
    SafeToFix,
    /// Index tree is not found in recent ancestor trees. The user has live
    /// staged content; `--fix` must not touch this.
    LiveStaged,
}

/// How stale working-tree files should be treated.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WorkingTreeDriftKind {
    /// All modified files' on-disk content matches blobs reachable from HEAD.
    /// Safe to restore with `git checkout HEAD -- <files>` — no work is lost.
    SafeToFix,
    /// At least one modified file has on-disk content not found in any recent
    /// ancestor's tree. The user has active edits; `--fix` must not touch this.
    LiveEdits,
}

/// Discriminator for [`CheckViolation::WorkweaveTreeIntegrity`] findings.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum WorkweaveTreeIntegrityKind {
    /// The marker's `parent:` path no longer exists on disk. The workweave's
    /// parent was retired or deleted out-of-band (a crash mid-adopt, or a
    /// hand-deletion) while this child remained. Bare `rwv sync-to` would
    /// otherwise mis-fire; instead it now surfaces friendly doctor-remediation
    /// text. Auto-fixable: `rwv doctor --fix` re-points `parent` to primary
    /// (which always exists). Normal retire/delete adopts children before the
    /// parent is destroyed, so this only arises off the happy path.
    DanglingParent {
        /// The missing parent path recorded in the marker.
        parent_path: PathBuf,
    },
    /// A parent-chain anomaly: cycle, parent==self, or the parent marker's
    /// project differs from this workweave's project. Cannot arise from
    /// `rwv workweave create`; can arise from hand-edited markers or
    /// directory copies. Report-only.
    ParentChainAnomaly {
        /// Short human-readable description of the anomaly.
        detail: String,
    },
    /// A directory under `.workweaves/` that has no `.rwv-workweave` marker
    /// file at all. It may be an orphaned directory from a failed create, a
    /// manually placed directory, or a remnant of a deleted workweave.
    /// Report-only.
    UnregisteredDir,
    /// The marker's `primary:` path does not resolve to the workspace this
    /// scan was started from (e.g. an rsync'd workweave whose marker still
    /// points at the origin machine's absolute path). Report-only.
    ForeignPrimary {
        /// The primary path recorded in the marker (unresolved).
        marker_primary: PathBuf,
    },
}

/// Discriminator for [`CheckViolation::Provenance`] findings.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceKind {
    /// The clone's `origin` remote URL differs from the URL recorded in the
    /// manifest. Until reconciled, pushes may publish to the wrong remote.
    /// Warning severity; report-only.
    ///
    /// Note: reference-role repos may intentionally point at a different
    /// remote (e.g. a local mirror). `is_reference_role` is `true` when the
    /// manifest records `role: reference` so the human-facing message can
    /// call out this nuance.
    OriginUrlMismatch {
        /// The URL recorded in the manifest (`rwv.yaml`).
        manifest_url: String,
        /// The actual fetch URL of the `origin` remote on disk.
        actual_url: String,
        /// `true` when the manifest entry carries `role: reference`.
        /// Reference-role repos may intentionally use a different remote
        /// (e.g. a local mirror), so the violation message notes this to
        /// help the operator decide whether to act.
        is_reference_role: bool,
    },
    /// The SHA pinned in `rwv.lock` is absent from the clone's object store.
    /// The canonical store is missing the pinned revision; refresh it from
    /// its remote (run a fetch — not a sync — to recover). Error severity;
    /// report-only.
    LockShaUnreachable {
        /// The SHA pinned in `rwv.lock` that cannot be found locally.
        sha: String,
    },
}

/// Discriminator for [`CheckViolation::CloneTopology`] findings.
///
/// The four sub-kinds enumerate the ways the bottom tier of the stability
/// stack
/// ([clone-topology.md](../../docs/explanation/joints/clone-topology.md))
/// can break: a manifest repo's slot at `<weave>/<repo_path>` must be a
/// "canonical store" (a full clone), and every workweave checkout
/// `<workweave>/<repo_path>` must be a linked workspace whose VCS common
/// store resolves to that canonical store. Each variant names a distinct
/// way the on-disk shape diverges from that spec.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum CloneTopologyKind {
    /// A full clone (its own canonical store) is hosted under `.workweaves/`
    /// instead of at the manifest's canonical slot. The inverted-primary
    /// case: the canonical store has migrated into one workweave and other
    /// workweaves' checkouts link into *it*, not into `<weave>/<repo_path>`.
    ///
    /// Reference-alias carve-out: a symlinked `reference` checkout (a
    /// `CheckoutKind::ReferenceAlias`, i.e. the workweave path is itself a
    /// symlink to the canonical store) is *not* a standalone store — it is the
    /// single canonical store viewed through a symlink, which upholds the
    /// single-canonical-store invariant by identity. The scan excludes it
    /// before this check. A *real* standalone store inside a workweave is a
    /// real directory (not a symlink) and still fires this finding.
    StandaloneInWorkweave {
        /// Absolute path of the standalone canonical store under
        /// `.workweaves/`.
        store_path: PathBuf,
    },
    /// The workspace at `<weave>/<repo_path>` is a full clone (its
    /// canonical store sits under itself), but one or more of this weave's
    /// workweave checkouts of the same repo resolve to a *different*
    /// canonical store. The weave-path clone publishes a separate object
    /// DAG nobody syncs to; push/pull becomes asymmetric and silent.
    DisconnectedWeaveClone {
        /// Absolute path of the canonical store at the weave slot (the
        /// "disconnected" one).
        weave_store_path: PathBuf,
        /// Absolute path of a representative store one of the workweave
        /// checkouts actually uses (the one this weave clone is
        /// disconnected from).
        other_store_path: PathBuf,
    },
    /// A linked worktree under `.workweaves/<workweave>/<repo_path>` whose
    /// canonical store is not the weave canonical at `<weave>/<repo_path>`.
    /// The shared-DAG invariant between the canonical and the workweave is
    /// broken: commits made here land in a different object store than the
    /// canonical, and merged-checks across the two answer "no" silently.
    WrongParentWorktree {
        /// Absolute path of the canonical store this workweave checkout
        /// should be linked into (`<weave>/<repo_path>/.git`).
        expected_store_path: PathBuf,
        /// Absolute path of the canonical store this workweave checkout
        /// is actually linked into.
        actual_store_path: PathBuf,
    },
    /// The weave path `<weave>/<repo_path>` itself is a linked worktree of
    /// some other clone — full inversion: there is no canonical store at
    /// the manifest slot, and the workspace there shares its DAG with
    /// whichever clone hosts the actual store.
    WeaveCloneIsWorktree {
        /// Absolute path of the canonical store this slot is linked into.
        actual_store_path: PathBuf,
    },
}
/// Discriminator for [`CheckViolation::BranchDiscipline`] findings.
///
/// Three groupings, mirroring the three checks in the spec:
///
/// * (a) workweave-branch — a workweave checkout is on the wrong branch:
///   [`SharedBranch`](Self::SharedBranch),
///   [`ForeignEphemeral`](Self::ForeignEphemeral),
///   [`Detached`](Self::Detached). Report-only.
/// * (b) ephemeral-at-primary — the canonical clone is on an ephemeral
///   `<project>--<name>/...` branch:
///   [`EphemeralAtPrimary`](Self::EphemeralAtPrimary). Report-only.
/// * (c) stale-ephemeral-branches — a `<project>--<name>/...` branch
///   exists in a canonical clone but workweave `<name>` no longer exists
///   on disk: [`StaleEphemeralBranchSafe`](Self::StaleEphemeralBranchSafe)
///   (auto-fixable by `--fix`) or
///   [`StaleEphemeralBranchLive`](Self::StaleEphemeralBranchLive)
///   (carries unique commits; never auto-deleted). The safe/live split
///   applies the doctrine in `docs/explanation/joints/shared-refs-drift.md`
///   to refs: a tip that is an ancestor of the primary's tracking-branch
///   tip carries no unique work and is safely removable; a tip with
///   commits not reachable from the primary is live work and must be left
///   alone.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum BranchDisciplineKind {
    /// (a) The workweave checkout is on a non-ephemeral branch (e.g. `main`).
    ///
    /// Caused by `git switch main` inside a workweave or by a bare clone
    /// that was never moved to an ephemeral branch. The fixture for this
    /// sub-kind exercises the bare-main-in-workweave case from the spec's
    /// acceptance criteria: the violation must flag from creation, before
    /// any commit lands. Report-only.
    ///
    /// Reference-alias carve-out: a symlinked `reference` checkout (a
    /// `CheckoutKind::ReferenceAlias`) legitimately shares the canonical
    /// store's non-ephemeral branch (e.g. `main`) — it has no per-workweave
    /// ephemeral branch by design, because it is the canonical store viewed
    /// through a symlink. The I3 branch-discipline scan skips such aliases, so
    /// they never fire this finding. A `reference` repo created with
    /// `--worktree-references` is a real worktree (`CheckoutKind::Worktree`) on
    /// its own ephemeral branch and is checked normally.
    SharedBranch {
        /// The branch currently checked out (e.g. `main`).
        actual_branch: String,
        /// The expected ephemeral prefix (`<project>--<workweave>`).
        expected_prefix: String,
    },
    /// (a) The workweave checkout is on an ephemeral branch named for a
    /// *different* workweave (the prefix `<project>--<other>/` differs
    /// from the expected `<project>--<workweave>/`). Report-only.
    ForeignEphemeral {
        /// The branch currently checked out.
        actual_branch: String,
        /// The expected ephemeral prefix (`<project>--<workweave>`).
        expected_prefix: String,
    },
    /// (a) The workweave checkout is in detached-HEAD state — HEAD points
    /// directly at a commit instead of a named branch. Detached HEAD
    /// breaks the merged-check and ref-namespace invariants in
    /// `clone-topology.md`. Report-only.
    Detached,
    /// (b) The canonical clone is checked out on an ephemeral
    /// `<project>--<name>/...` branch — the inverse of (a). Either the
    /// canonical was moved onto a workweave branch, or a workweave
    /// directory was deleted and the canonical was left holding its
    /// ephemeral branch. Report-only.
    EphemeralAtPrimary {
        /// The branch currently checked out on the canonical.
        actual_branch: String,
    },
    /// (c) A `<project>--<name>/...` branch in the canonical clone whose
    /// workweave `<name>` no longer exists on disk, and whose tip is an
    /// ancestor of the primary tracking branch's tip (no unique commits).
    /// Safe-class per the shared-refs-drift doctrine — `--fix` may delete
    /// the branch with no information loss.
    StaleEphemeralBranchSafe {
        /// The full branch name (e.g. `foundations--feat-a/main`).
        branch: String,
        /// The workweave name parsed out of the branch (the `<name>`
        /// component); the directory `.workweaves/<project>--<name>` is
        /// absent on disk.
        workweave_name: String,
    },
    /// (c) A `<project>--<name>/...` branch in the canonical clone whose
    /// workweave `<name>` no longer exists on disk, but whose tip carries
    /// commits not reachable from the primary tracking branch's tip
    /// (unique work). Live-class per the shared-refs-drift doctrine —
    /// report-only; `--fix` never touches this. The operator decides
    /// whether to land the commits, archive the branch, or delete it.
    StaleEphemeralBranchLive {
        /// The full branch name.
        branch: String,
        /// The workweave name parsed out of the branch.
        workweave_name: String,
        /// The branch tip SHA, surfaced so the operator can recover the
        /// commits before deleting (e.g. `git log <tip_sha>`).
        tip_sha: String,
    },
}

// ---------------------------------------------------------------------------
// ViolationOutput — wire-format mirror of CheckViolation for `--json`
// ---------------------------------------------------------------------------
//
// The internal `CheckViolation` enum carries a `RepoPath` (manifest-relative).
// The wire shape needs both `path` (manifest-relative string) and
// `absolute_path` (resolved against the workspace root or workweave dir),
// which the internal type cannot supply alone. We mirror the variants here
// and convert at serialize time via [`ViolationOutput::from_violation`].
//
// The kebab-case tag mapping:
//     OrphanedClone       -> "orphaned-clone"
//     DanglingReference   -> "dangling-reference"
//     MissingRole         -> "missing-role"
//     StaleLock           -> "stale-lock"
//     WorkweaveDrift      -> "workweave-drift"  (sub-kind via `DriftKind`)
//     IndexDrift          -> "index-drift"      (sub-kind via `IndexDriftKind`)
//     WorkingTreeDrift    -> "working-tree-drift" (sub-kind via `WorkingTreeDriftKind`)
//     MissingReplayExclusion -> "missing-replay-exclusion"
//     MissingCanonicalClone  -> "missing-canonical-clone"

/// One violation as it appears in `rwv doctor --json` output.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ViolationOutput {
    OrphanedClone {
        path: String,
        absolute_path: String,
    },
    DanglingReference {
        path: String,
        absolute_path: String,
        project: String,
    },
    MissingRole {
        path: String,
        absolute_path: String,
        project: String,
    },
    StaleLock {
        path: String,
        absolute_path: String,
        project: String,
        locked: String,
        actual: String,
    },
    WorkweaveDrift {
        path: String,
        absolute_path: String,
        workweave: String,
        #[serde(rename = "sub_kind")]
        sub_kind: DriftKind,
    },
    IndexDrift {
        path: String,
        absolute_path: String,
        /// `None` for the primary weave.
        workweave: Option<String>,
        #[serde(rename = "sub_kind")]
        sub_kind: IndexDriftKind,
    },
    WorkingTreeDrift {
        path: String,
        absolute_path: String,
        /// `None` for the primary weave.
        workweave: Option<String>,
        #[serde(rename = "sub_kind")]
        sub_kind: WorkingTreeDriftKind,
    },
    MissingReplayExclusion {
        project: String,
    },
    LegacyRolePrimary {
        project: String,
        manifest_path: String,
    },
    DanglingActiveProject {
        project: String,
        missing_dir: String,
    },
    LegacyWorkweaveMarker {
        marker_path: String,
        primary: String,
    },
    UnparseableProject {
        project: String,
        manifest_path: String,
        /// Free-form display string of the YAML parse error. Named `message`
        /// (not `error`) to signal this is display text, not a typed discriminant.
        message: String,
    },
    WorkweaveTreeIntegrity {
        /// Absolute path to the workweave directory (or its marker file for
        /// file-level findings).
        workweave_dir: String,
        /// Discriminator for the specific anomaly detected.
        #[serde(rename = "sub_kind")]
        sub_kind: WorkweaveTreeIntegrityKind,
    },
    Provenance {
        /// Manifest-relative path to the affected repo.
        path: String,
        /// Absolute path to the affected repo on disk.
        absolute_path: String,
        /// Project the affected repo belongs to.
        project: String,
        /// Discriminator for the specific provenance anomaly.
        #[serde(rename = "sub_kind")]
        sub_kind: ProvenanceKind,
    },
    CloneTopology {
        /// Manifest-relative repo path (e.g. `github/cwalv/tmuxcc-broker`).
        path: String,
        /// Absolute path of the offending workspace (canonical slot or
        /// workweave checkout, per sub-kind semantics).
        absolute_path: String,
        /// Discriminator for the specific topology anomaly.
        #[serde(rename = "sub_kind")]
        sub_kind: CloneTopologyKind,
    },
    BranchDiscipline {
        /// Absolute path to the repo checkout where the violation was
        /// found (workweave checkout for (a), canonical clone for (b)/(c)).
        repo_path: String,
        /// Discriminator for the specific branch-discipline anomaly.
        #[serde(rename = "sub_kind")]
        sub_kind: BranchDisciplineKind,
    },
    StaleWorktreeRegistration {
        path: String,
        absolute_path: String,
        /// `None` for the primary weave.
        workweave: Option<String>,
        /// Absolute path of the missing worktree directory.
        missing_path: String,
    },
    StaleOpState {
        /// Absolute path to the workspace dir that holds the `.rwv-op` file.
        workspace_dir: String,
        /// Raw `started_at` string from the op-state file (RFC3339 UTC).
        started_at: String,
    },
    DeadOpLease {
        /// Absolute path to the workspace dir holding the dangling lease.
        workspace_dir: String,
        /// Op id recorded in the lease.
        op_id: String,
        /// Owner workspace the lease pointed at.
        recorded_owner: String,
        /// Discriminator for the specific dead-lease shape.
        #[serde(rename = "sub_kind")]
        sub_kind: DeadOpLeaseKind,
        /// RFC3339 UTC timestamp at which the lease was written.
        /// `None` for old lease files. Observability-only.
        #[serde(skip_serializing_if = "Option::is_none")]
        created_at: Option<String>,
    },
    OrphanedSavepoint {
        path: String,
        absolute_path: String,
        /// `None` for the primary weave.
        workweave: Option<String>,
        /// Opaque op-id from the savepoint ref's trailing path component.
        op_id: String,
        /// Safe-vs-live classification.
        #[serde(rename = "sub_kind")]
        sub_kind: OrphanedSavepointKind,
    },
    /// See [`CheckViolation::CargoVersionSkew`].
    CargoVersionSkew {
        /// Registry crate name.
        crate_name: String,
        /// Per-member requirement strings (post-`workspace = true`
        /// indirection). Sorted for stable output.
        occurrences: Vec<CargoSkewOccurrenceOutput>,
    },
    /// See [`CheckViolation::CargoPatchShadowing`].
    CargoPatchShadowing {
        /// Weave-level file (Cargo.toml or .cargo/config.toml) that
        /// carries the shadowed patch entry.
        weave_config: String,
        /// Member-level `.cargo/config.toml` that wins per cargo's
        /// closest-config-wins-per-key shadowing.
        member_config: String,
        /// Registry sub-table name (e.g. `crates-io`).
        registry: String,
        /// The specific crate name whose key collides.
        crate_name: String,
    },
    /// See [`CheckViolation::MissingCanonicalClone`].
    MissingCanonicalClone {
        /// Manifest-relative path to the affected repo (same value as
        /// [`CheckViolation::MissingCanonicalClone::repo`]).
        path: String,
        /// Absolute path of the worktree checkout in the workweave.
        absolute_path: String,
        /// Workweave name.
        workweave: String,
        /// Absolute path of the canonical clone directory that is absent.
        canonical_path: String,
    },

    /// See [`CheckViolation::UninitializedSubmodule`].
    UninitializedSubmodule {
        /// Absolute path to the repo worktree that has uninitialized submodules.
        absolute_path: String,
        /// Manifest-relative path to the repo.
        path: String,
        /// Workweave name.
        workweave: String,
        /// Submodule paths (relative to the repo root) that are empty on disk.
        empty_paths: Vec<String>,
    },
}

/// Wire representation of [`crate::integrations::cargo_workspace::CargoSkewOccurrence`].
///
/// Kept separate so the internal type stays free of serde/schemars deps
/// and the wire shape is a single-source-of-truth definition here.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct CargoSkewOccurrenceOutput {
    /// Weave-relative member path.
    pub member: String,
    /// Requirement string (post `workspace = true` indirection).
    pub requirement: String,
}

impl ViolationOutput {
    /// Convert an internal [`CheckViolation`] into its wire-format
    /// counterpart, resolving `path` against `workspace_dir` for
    /// non-workweave variants and against `workweave_dirs` for
    /// workweave-scoped variants.
    pub fn from_violation(
        violation: CheckViolation,
        workspace_dir: &Path,
        workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
    ) -> Self {
        fn abs(workspace_dir: &Path, repo: &RepoPath) -> String {
            workspace_dir
                .join(repo.as_path())
                .to_string_lossy()
                .into_owned()
        }
        fn abs_in(
            workweave: &Option<WorkweaveName>,
            workspace_dir: &Path,
            workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
            repo: &RepoPath,
        ) -> String {
            match workweave {
                Some(ww) => match workweave_dirs.get(ww) {
                    Some(dir) => dir.join(repo.as_path()).to_string_lossy().into_owned(),
                    None => workspace_dir
                        .join(repo.as_path())
                        .to_string_lossy()
                        .into_owned(),
                },
                None => workspace_dir
                    .join(repo.as_path())
                    .to_string_lossy()
                    .into_owned(),
            }
        }

        match violation {
            CheckViolation::OrphanedClone { path } => Self::OrphanedClone {
                absolute_path: abs(workspace_dir, &path),
                path: path.to_string(),
            },
            CheckViolation::DanglingReference { project, repo } => Self::DanglingReference {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.to_string(),
            },
            CheckViolation::MissingRole { project, repo } => Self::MissingRole {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.to_string(),
            },
            CheckViolation::StaleLock {
                project,
                repo,
                locked,
                actual,
            } => Self::StaleLock {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.to_string(),
                locked: locked.display_str().to_owned(),
                actual: actual.display_str().to_owned(),
            },
            CheckViolation::WorkweaveDrift {
                workweave,
                kind,
                repo,
            } => {
                let dir_for_ww = workweave_dirs
                    .get(&workweave)
                    .cloned()
                    .unwrap_or_else(|| workspace_dir.to_path_buf());
                Self::WorkweaveDrift {
                    absolute_path: dir_for_ww
                        .join(repo.as_path())
                        .to_string_lossy()
                        .into_owned(),
                    path: repo.to_string(),
                    workweave: workweave.to_string(),
                    sub_kind: kind,
                }
            }
            CheckViolation::IndexDrift {
                workweave,
                repo,
                kind,
            } => Self::IndexDrift {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.map(|w| w.to_string()),
                sub_kind: kind,
            },
            CheckViolation::WorkingTreeDrift {
                workweave,
                repo,
                kind,
            } => Self::WorkingTreeDrift {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.map(|w| w.to_string()),
                sub_kind: kind,
            },
            CheckViolation::MissingReplayExclusion { project } => Self::MissingReplayExclusion {
                project: project.to_string(),
            },
            CheckViolation::LegacyRolePrimary {
                project,
                manifest_path,
            } => Self::LegacyRolePrimary {
                project: project.to_string(),
                manifest_path: manifest_path.to_string_lossy().into_owned(),
            },
            CheckViolation::DanglingActiveProject {
                project,
                missing_dir,
            } => Self::DanglingActiveProject {
                project: project.to_string(),
                missing_dir: missing_dir.to_string_lossy().into_owned(),
            },
            CheckViolation::LegacyWorkweaveMarker {
                marker_path,
                primary,
            } => Self::LegacyWorkweaveMarker {
                marker_path: marker_path.to_string_lossy().into_owned(),
                primary: primary.to_string_lossy().into_owned(),
            },
            CheckViolation::UnparseableProject {
                project,
                manifest_path,
                message,
            } => Self::UnparseableProject {
                project: project.to_string(),
                manifest_path: manifest_path.to_string_lossy().into_owned(),
                message,
            },
            CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir,
                sub_kind,
            } => Self::WorkweaveTreeIntegrity {
                workweave_dir: workweave_dir.to_string_lossy().into_owned(),
                sub_kind,
            },
            CheckViolation::Provenance {
                project,
                repo,
                sub_kind,
            } => Self::Provenance {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.to_string(),
                sub_kind,
            },
            CheckViolation::CloneTopology {
                workspace_path,
                repo,
                sub_kind,
            } => Self::CloneTopology {
                absolute_path: workspace_path.to_string_lossy().into_owned(),
                path: repo.to_string(),
                sub_kind,
            },
            CheckViolation::BranchDiscipline {
                repo_path,
                sub_kind,
            } => Self::BranchDiscipline {
                repo_path: repo_path.to_string_lossy().into_owned(),
                sub_kind,
            },
            CheckViolation::StaleWorktreeRegistration {
                workweave,
                repo,
                missing_path,
            } => Self::StaleWorktreeRegistration {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.map(|w| w.to_string()),
                missing_path: missing_path.to_string_lossy().into_owned(),
            },
            CheckViolation::StaleOpState {
                workspace_dir: ws_dir,
                started_at,
            } => Self::StaleOpState {
                workspace_dir: ws_dir.to_string_lossy().into_owned(),
                started_at,
            },
            CheckViolation::DeadOpLease {
                workspace_dir: ws_dir,
                op_id,
                recorded_owner,
                sub_kind,
                created_at,
            } => Self::DeadOpLease {
                workspace_dir: ws_dir.to_string_lossy().into_owned(),
                op_id,
                recorded_owner: recorded_owner.to_string_lossy().into_owned(),
                sub_kind,
                created_at,
            },
            CheckViolation::OrphanedSavepoint {
                workweave,
                repo,
                op_id,
                sub_kind,
            } => Self::OrphanedSavepoint {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.map(|w| w.to_string()),
                op_id,
                sub_kind,
            },
            CheckViolation::CargoVersionSkew {
                crate_name,
                occurrences,
            } => Self::CargoVersionSkew {
                crate_name,
                occurrences: occurrences
                    .into_iter()
                    .map(|o| CargoSkewOccurrenceOutput {
                        member: o.member,
                        requirement: o.requirement,
                    })
                    .collect(),
            },
            CheckViolation::CargoPatchShadowing {
                weave_config,
                member_config,
                registry,
                crate_name,
            } => Self::CargoPatchShadowing {
                weave_config: weave_config.to_string_lossy().into_owned(),
                member_config: member_config.to_string_lossy().into_owned(),
                registry,
                crate_name,
            },
            CheckViolation::MissingCanonicalClone {
                workweave,
                repo,
                canonical_path,
            } => {
                let ww_dir = workweave_dirs
                    .get(&workweave)
                    .cloned()
                    .unwrap_or_else(|| workspace_dir.to_path_buf());
                Self::MissingCanonicalClone {
                    absolute_path: ww_dir.join(repo.as_path()).to_string_lossy().into_owned(),
                    path: repo.to_string(),
                    workweave: workweave.to_string(),
                    canonical_path: canonical_path.to_string_lossy().into_owned(),
                }
            }
            CheckViolation::UninitializedSubmodule {
                workweave,
                repo,
                empty_paths,
            } => {
                let ww_dir = workweave_dirs.get(&workweave);
                let absolute_path = match ww_dir {
                    Some(dir) => dir.join(repo.as_path()).to_string_lossy().into_owned(),
                    None => workspace_dir
                        .join(repo.as_path())
                        .to_string_lossy()
                        .into_owned(),
                };
                Self::UninitializedSubmodule {
                    absolute_path,
                    path: repo.to_string(),
                    workweave: workweave.as_str().to_string(),
                    empty_paths,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy-role-primary scanning
// ---------------------------------------------------------------------------

/// One project manifest carrying the legacy `role: primary` spelling.
///
/// Carries both the project name and the absolute manifest path so the
/// finding can be reported (without --fix) and rewritten (with --fix)
/// without re-walking the workspace.
#[derive(Debug, Clone)]
pub struct LegacyRolePrimaryManifest {
    pub project: ProjectName,
    pub manifest_path: PathBuf,
}

/// Walk every `projects/*/rwv.yaml` under `workspace_dir` and collect
/// manifests that contain the legacy `role: primary` spelling.
///
/// Pre-parse text scan — the doctor needs to detect the legacy spelling
/// *before* `Project::from_dir`, since the parser rejects it. Without
/// this scan, the only signal would be the parse error from
/// `Project::from_dir`, which doesn't fan out across all manifests in
/// the workspace.
pub fn scan_workspace_for_legacy_role_primary(
    workspace_dir: &Path,
) -> Vec<LegacyRolePrimaryManifest> {
    let projects_dir = workspace_dir.join("projects");
    let mut found = Vec::new();
    if !projects_dir.is_dir() {
        return found;
    }
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return found,
    };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        scan_project_dir_for_legacy(&projects_dir, &project_dir, &mut found);
    }
    found
}

/// Recursively walk a project directory in `projects/` for `rwv.yaml`
/// files using `role: primary`. Project names are derived as the
/// path relative to `projects/` (so `projects/chatly/web-app/rwv.yaml`
/// yields project name `chatly/web-app`), matching the existing
/// nested-project convention used by `Project::from_dir`.
fn scan_project_dir_for_legacy(
    projects_dir: &Path,
    project_dir: &Path,
    out: &mut Vec<LegacyRolePrimaryManifest>,
) {
    let manifest_path = project_dir.join("rwv.yaml");
    if manifest_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&manifest_path) {
            if crate::manifest::manifest_has_legacy_role_primary(&content) {
                let project_name = project_dir
                    .strip_prefix(projects_dir)
                    .unwrap_or(project_dir)
                    .to_string_lossy()
                    .into_owned();
                out.push(LegacyRolePrimaryManifest {
                    project: ProjectName::new(project_name),
                    manifest_path,
                });
            }
        }
    }
    // Recurse into subdirectories for the `projects/<owner>/<repo>` nested
    // case. Skip `.git` and similar hidden directories.
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                scan_project_dir_for_legacy(projects_dir, &path, out);
            }
        }
    }
}

/// Apply the `rwv doctor --fix` migration to a single manifest path.
///
/// Idempotent: if no `role: primary` lines remain, the file is not
/// rewritten and the returned count is `0`. Returns the number of
/// rewritten lines so the caller can print a meaningful "[fixed]" line.
pub fn fix_legacy_role_primary(manifest_path: &Path) -> anyhow::Result<usize> {
    let content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {} for --fix", manifest_path.display()))?;
    let (new_content, count) = crate::manifest::migrate_legacy_role_primary(&content);
    if count > 0 {
        std::fs::write(manifest_path, new_content)
            .with_context(|| format!("failed to write {} during --fix", manifest_path.display()))?;
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Legacy-workweave-marker scanning and fixing
// ---------------------------------------------------------------------------

/// One workweave directory whose `.rwv-workweave` file is missing `parent:`.
#[derive(Debug, Clone)]
pub struct LegacyWorkweaveMarkerFile {
    /// Absolute path to the `.rwv-workweave` file.
    pub marker_path: PathBuf,
    /// The `primary:` value read from the file (used as the backfill value).
    pub primary: PathBuf,
}

/// Walk the workweave parent directory and collect `.rwv-workweave` files that
/// are missing the required `parent:` field.
///
/// A marker is "legacy" if the YAML is valid but `parent:` is absent or null.
/// Files that fail to parse at all are not included (they are a different
/// failure mode).
pub fn scan_for_legacy_workweave_markers(ws_root: &Path) -> Vec<LegacyWorkweaveMarkerFile> {
    let parent_dir = crate::workweave::workweave_parent_pub(ws_root);
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(&parent_dir) {
        Ok(e) => e,
        Err(_) => return found,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let marker_path = dir.join(".rwv-workweave");
        if !marker_path.is_file() {
            continue;
        }
        let content = match std::fs::read_to_string(&marker_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let raw: serde_yaml::Value = match serde_yaml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue, // unparseable — not our concern here
        };
        // Legacy if `parent` is absent or null.
        if raw.get("parent").map(|v| v.is_null()).unwrap_or(true) {
            // Extract primary path.
            if let Some(primary_str) = raw.get("primary").and_then(|v| v.as_str()) {
                found.push(LegacyWorkweaveMarkerFile {
                    marker_path,
                    primary: PathBuf::from(primary_str),
                });
            }
        }
    }
    found.sort_by(|a, b| a.marker_path.cmp(&b.marker_path));
    found
}

/// Append `parent: <primary>` to a legacy `.rwv-workweave` file.
///
/// Idempotent: if `parent:` is already present, the file is not rewritten.
/// Returns `true` if the file was rewritten, `false` if it was already
/// up to date.
pub fn fix_legacy_workweave_marker(finding: &LegacyWorkweaveMarkerFile) -> anyhow::Result<bool> {
    let content = std::fs::read_to_string(&finding.marker_path)
        .with_context(|| format!("failed to read {} for --fix", finding.marker_path.display()))?;
    let raw: serde_yaml::Value = serde_yaml::from_str(&content).with_context(|| {
        format!(
            "failed to re-parse {} for --fix",
            finding.marker_path.display()
        )
    })?;
    // Re-check: don't rewrite if already has a non-null parent.
    if !raw.get("parent").map(|v| v.is_null()).unwrap_or(true) {
        return Ok(false);
    }
    // Append the parent line. Using a simple string append preserves any
    // comments and existing key order, consistent with fix_legacy_role_primary.
    let primary_str = finding.primary.to_string_lossy();
    let line = format!("parent: {primary_str}\n");
    let new_content = if content.ends_with('\n') {
        format!("{content}{line}")
    } else {
        format!("{content}\n{line}")
    };
    std::fs::write(&finding.marker_path, new_content).with_context(|| {
        format!(
            "failed to write {} during --fix",
            finding.marker_path.display()
        )
    })?;
    Ok(true)
}

/// Re-point a `dangling-parent` workweave marker's `parent:` field to
/// `primary` (which always exists).
///
/// The `workweave-tree-integrity` / `dangling-parent` violation message
/// already tells the operator to "re-point parent to a valid workspace";
/// `--fix` gives that instruction a verb. Primary is the safe universal
/// target: it is guaranteed present, and lineage remains sound because a
/// workweave's unique work ultimately lands in primary regardless of the
/// intermediate chain.
///
/// `marker_dir` is the workweave directory whose `.rwv-workweave` will be
/// rewritten. Returns `true` if the marker was rewritten, `false` if the
/// parent already resolves (a race where the dangling condition healed before
/// the fix ran). `Err` only on I/O or a genuinely unreadable/legacy marker.
///
/// The `parent:` path is rewritten to `primary`'s canonical path; the marker's
/// other fields (`primary`, `project`) are preserved. Branch names are NOT
/// touched — they are creation-time namespaces, not lineage.
pub fn fix_dangling_parent(marker_dir: &Path, primary: &Path) -> anyhow::Result<bool> {
    let mut marker = crate::workspace::WorkweaveMarker::read(marker_dir)
        .with_context(|| {
            format!(
                "failed to read {}/.rwv-workweave for --fix",
                marker_dir.display()
            )
        })?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}/.rwv-workweave vanished before --fix could re-point it",
                marker_dir.display()
            )
        })?;

    // Race guard: if the parent already exists, the dangling condition healed
    // (e.g. the parent was recreated) — leave the marker alone.
    if marker.parent.exists() {
        return Ok(false);
    }

    let new_parent = primary
        .canonicalize()
        .unwrap_or_else(|_| primary.to_path_buf());
    marker.parent = new_parent;
    marker.write(marker_dir).with_context(|| {
        format!(
            "failed to write {}/.rwv-workweave during --fix",
            marker_dir.display()
        )
    })?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Replay-exclusion (.gitattributes) migration commit helper
// ---------------------------------------------------------------------------

/// Result of an rwv-authored .gitattributes commit attempt.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommitOutcome {
    /// The migration was staged and committed.
    Committed,
    /// The project repo had unrelated staged changes; the migration was
    /// left in the working tree unstaged (or with only `.gitattributes`
    /// staged) so the operator can review before committing. Skipping is
    /// the safe behaviour: never bundle a user's WIP with an rwv-authored
    /// fix.
    SkippedUnrelatedStaged,
    /// The migration produced no diff (e.g. race — the file was already
    /// on the new spelling by the time we tried to commit). Idempotent no-op.
    NothingToCommit,
}

/// Stage and commit the `.gitattributes` change written by
/// [`Vcs::set_replay_exclusion`] during the legacy `merge=ours` →
/// `merge=rwv-ours` migration.
///
/// Refuses to commit when the project repo has any staged change other than
/// `.gitattributes` — returns [`CommitOutcome::SkippedUnrelatedStaged`] and
/// leaves the working-tree migration in place. This mirrors
/// `lock::commit_lock_file`'s bundling refusal, adapted for `.gitattributes`.
///
/// Commit message follows the same convention as
/// `chore: add rwv.lock replay-exclusion` mentioned in
/// `sync::verify_replay_exclusion_invariant`'s fallback hint so the two
/// forms of the fix (auto and hand-run) sit on adjacent commits with
/// consistent framing.
pub(crate) fn commit_replay_exclusion_migration(
    project_dir: &Path,
) -> anyhow::Result<CommitOutcome> {
    use crate::git::git_command;

    // Check for unrelated staged content BEFORE we touch the index.
    // `git status --porcelain` porcelain-v1 lines are `XY path` where X is
    // the index status and Y is the worktree status. A staged unrelated
    // file has X != ' ' and path != ".gitattributes". Untracked files
    // (`??`) don't count — they've never been staged.
    let status_out = git_command()
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(project_dir)
        .output()
        .with_context(|| {
            format!(
                "failed to run git status --porcelain in {}",
                project_dir.display()
            )
        })?;
    if !status_out.status.success() {
        let stderr = String::from_utf8_lossy(&status_out.stderr);
        anyhow::bail!("git status failed: {}", stderr.trim());
    }
    let status_str = String::from_utf8_lossy(&status_out.stdout);
    let has_other_staged = status_str.lines().any(|line| {
        // Porcelain v1: 2-char status column, one space, then path.
        let bytes = line.as_bytes();
        if bytes.len() < 3 {
            return false;
        }
        let index_status = bytes[0];
        if index_status == b' ' || index_status == b'?' {
            return false;
        }
        // The path begins at byte 3. A rename appears as `R  old -> new`;
        // detecting only the "new" name after `-> ` is enough for the
        // bundling check since renames touching non-.gitattributes count
        // as unrelated staged work regardless of the old name.
        let path_part = line.get(3..).unwrap_or("").trim();
        let path = match path_part.split_once(" -> ") {
            Some((_, new_name)) => new_name,
            None => path_part,
        };
        path != ".gitattributes"
    });
    if has_other_staged {
        return Ok(CommitOutcome::SkippedUnrelatedStaged);
    }

    // Stage the migration.
    let add_out = git_command()
        .args(["add", ".gitattributes"])
        .current_dir(project_dir)
        .output()
        .with_context(|| format!("failed to run git add in {}", project_dir.display()))?;
    if !add_out.status.success() {
        let stderr = String::from_utf8_lossy(&add_out.stderr);
        anyhow::bail!("git add .gitattributes failed: {}", stderr.trim());
    }

    // `git diff --cached --quiet` exits 0 when nothing is staged.
    let nothing_staged = git_command()
        .args(["diff", "--cached", "--quiet"])
        .current_dir(project_dir)
        .output()
        .with_context(|| {
            format!(
                "failed to check staged changes in {}",
                project_dir.display()
            )
        })?
        .status
        .success();
    if nothing_staged {
        return Ok(CommitOutcome::NothingToCommit);
    }

    let commit_out = git_command()
        .args([
            "commit",
            "-m",
            "chore: migrate rwv.lock merge=ours → merge=rwv-ours (rwv doctor --fix)",
        ])
        .current_dir(project_dir)
        .output()
        .with_context(|| format!("failed to run git commit in {}", project_dir.display()))?;
    if !commit_out.status.success() {
        let stderr = String::from_utf8_lossy(&commit_out.stderr);
        anyhow::bail!("git commit failed: {}", stderr.trim());
    }
    Ok(CommitOutcome::Committed)
}

// ---------------------------------------------------------------------------
// Workweave-tree integrity scanning
// ---------------------------------------------------------------------------

/// Scan the workweave parent directory for `.rwv-workweave` marker tree
/// anomalies.
///
/// Checks performed:
///
/// 1. **`dangling-parent`** — marker's `parent:` path does not exist on disk.
///    Auto-fixable via `rwv doctor --fix` (re-points to primary); the other
///    three sub-kinds are report-only.
/// 2. **`parent-chain-anomaly`** — cycle (A→B→A…), parent==self, or the
///    parent marker's `project` differs from the child's `project`.
/// 3. **`unregistered-dir`** — a directory under `.workweaves/` that has no
///    `.rwv-workweave` marker file.
/// 4. **`foreign-primary`** — marker's `primary:` does not canonicalize to
///    `ws_root`.
///
/// Workweave directories are located via
/// [`crate::workweave::workweave_parent_pub`]. Only top-level entries under
/// the parent directory are scanned (children of nested workweaves are under
/// a different parent and will be picked up when doctor runs from those
/// workweaves, or via a recursive descent — but the spec calls for a single
/// flat scan at the primary's `.workweaves/` level).
pub fn scan_workweave_tree_integrity(ws_root: &Path) -> Vec<CheckViolation> {
    let parent_dir = crate::workweave::workweave_parent_pub(ws_root);
    let ws_canonical = ws_root
        .canonicalize()
        .unwrap_or_else(|_| ws_root.to_path_buf());

    let mut violations = Vec::new();

    let entries = match std::fs::read_dir(&parent_dir) {
        Ok(e) => e,
        Err(_) => return violations, // parent dir missing → nothing to check
    };

    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    // Phase 1: per-directory checks (unregistered-dir, foreign-primary,
    // dangling-parent).  Collect marker data for phase-2 chain analysis.
    struct MarkerEntry {
        dir: PathBuf,
        project: crate::manifest::ProjectName,
        parent: PathBuf,
    }
    let mut marker_entries: Vec<MarkerEntry> = Vec::new();

    for dir in &dirs {
        let marker_path = dir.join(".rwv-workweave");

        if !marker_path.exists() {
            // No marker file at all → unregistered directory.
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: dir.clone(),
                sub_kind: WorkweaveTreeIntegrityKind::UnregisteredDir,
            });
            continue;
        }

        // Try to parse the marker. Legacy markers (missing `parent:`) are
        // handled by the separate legacy-workweave-marker check; we skip
        // them here (they'll get a `LegacyWorkweaveMarker` violation
        // instead, which directs the operator to `--fix`).
        let marker = match crate::workspace::WorkweaveMarker::read(dir) {
            Ok(Some(m)) => m,
            Ok(None) => {
                // Marker file exists but `read()` returned None — shouldn't
                // happen (None means the file was absent), but be defensive.
                violations.push(CheckViolation::WorkweaveTreeIntegrity {
                    workweave_dir: dir.clone(),
                    sub_kind: WorkweaveTreeIntegrityKind::UnregisteredDir,
                });
                continue;
            }
            Err(_) => {
                // Legacy marker (missing `parent:`) — already reported by
                // scan_for_legacy_workweave_markers; don't double-report.
                continue;
            }
        };

        // Foreign-primary check: marker's `primary` must resolve to ws_root.
        let marker_primary_canonical = marker
            .primary
            .canonicalize()
            .unwrap_or_else(|_| marker.primary.clone());
        if marker_primary_canonical != ws_canonical {
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: dir.clone(),
                sub_kind: WorkweaveTreeIntegrityKind::ForeignPrimary {
                    marker_primary: marker.primary.clone(),
                },
            });
            // A foreign-primary marker's `parent` field refers to another
            // machine's paths; chain analysis against our on-disk tree would
            // produce noise. Skip further checks for this directory.
            continue;
        }

        // Dangling-parent check: the parent path must exist on disk.
        if !marker.parent.exists() {
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: dir.clone(),
                sub_kind: WorkweaveTreeIntegrityKind::DanglingParent {
                    parent_path: marker.parent.clone(),
                },
            });
            // Even with a dangling parent we can still collect the entry
            // for the cycle/cross-project check using what we have.
        }

        marker_entries.push(MarkerEntry {
            dir: dir.clone(),
            project: marker.project.clone(),
            parent: marker.parent.clone(),
        });
    }

    // Phase 2: parent-chain anomaly detection.
    //
    // Walk from each workweave's `parent` field upward.  We look for:
    //   (a) parent == self (the directory points to itself)
    //   (b) cycle (visited set contains a node we reach again)
    //   (c) cross-project: the parent directory has a marker whose `project`
    //       differs from the starting workweave's `project`.
    //
    // Only workweaves in this parent directory are checked; parents that
    // resolve to ws_root (the primary) are the normal healthy base case and
    // are not followed.
    //
    // Build a lookup: canonical_dir → (project, parent).
    let dir_lookup: std::collections::HashMap<PathBuf, (crate::manifest::ProjectName, PathBuf)> =
        marker_entries
            .iter()
            .filter_map(|e| {
                let canon = e.dir.canonicalize().ok()?;
                Some((canon, (e.project.clone(), e.parent.clone())))
            })
            .collect();

    for entry in &marker_entries {
        let dir_canon = match entry.dir.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };

        // (a) parent == self
        let parent_canon = entry
            .parent
            .canonicalize()
            .unwrap_or_else(|_| entry.parent.clone());
        if parent_canon == dir_canon {
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: entry.dir.clone(),
                sub_kind: WorkweaveTreeIntegrityKind::ParentChainAnomaly {
                    detail: "marker `parent` points to the workweave itself (self-loop)".into(),
                },
            });
            continue;
        }

        // Walk the parent chain looking for cycles and cross-project anomalies.
        let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        visited.insert(dir_canon.clone());

        let mut current_parent = parent_canon.clone();
        loop {
            // Reached the primary weave root → healthy base case.
            if current_parent == ws_canonical {
                break;
            }

            if visited.contains(&current_parent) {
                // Cycle detected.
                violations.push(CheckViolation::WorkweaveTreeIntegrity {
                    workweave_dir: entry.dir.clone(),
                    sub_kind: WorkweaveTreeIntegrityKind::ParentChainAnomaly {
                        detail: format!(
                            "marker `parent` chain contains a cycle through `{}`",
                            current_parent.display()
                        ),
                    },
                });
                break;
            }
            visited.insert(current_parent.clone());

            // Check for cross-project parent.
            if let Some((parent_project, next_parent)) = dir_lookup.get(&current_parent) {
                if parent_project.as_str() != entry.project.as_str() {
                    violations.push(CheckViolation::WorkweaveTreeIntegrity {
                        workweave_dir: entry.dir.clone(),
                        sub_kind: WorkweaveTreeIntegrityKind::ParentChainAnomaly {
                            detail: format!(
                                "marker `parent` (`{}`) belongs to project `{}` but this \
                                 workweave is project `{}`",
                                current_parent.display(),
                                parent_project.as_str(),
                                entry.project.as_str()
                            ),
                        },
                    });
                    break;
                }
                // Keep climbing.
                current_parent = next_parent
                    .canonicalize()
                    .unwrap_or_else(|_| next_parent.clone());
            } else {
                // Parent is not in our lookup (not one of the dirs we
                // scanned). It may be a nested workweave's own child
                // workweave (out of scope of this flat scan), or a path
                // that no longer exists (already caught by dangling-parent
                // above). Stop here.
                break;
            }
        }
    }

    violations
}

// ---------------------------------------------------------------------------
// Provenance scanning
// ---------------------------------------------------------------------------

/// Run the cargo version-skew + patch-shadowing scans for a single project
/// and translate their outputs to [`CheckViolation`]s.
///
/// The scan is opt-in through the same enablement rules as the cargo-workspace
/// integration itself (silent no-op if disabled, or if no cargo work is
/// present in the workspace). The two scans are purely additive to doctor:
/// they never fail activation, never modify state, and always surface as
/// warnings so `rwv doctor` exit-status stays 0 by default.
///
/// Nested-workspace repos are still hard-errored at activation time, but the
/// scanner reads them anyway (see [`crate::integrations::cargo_workspace::CargoWorkspace::scan_members`]).
/// The grok-build test case (85 crates under a single nested workspace) is
/// exactly the shape this covers.
pub fn scan_cargo_ecosystem(
    ctx: &crate::integration::IntegrationContext,
) -> anyhow::Result<Vec<CheckViolation>> {
    use crate::integration::is_enabled;
    use crate::integrations::cargo_workspace::CargoWorkspace;
    use crate::manifest::CargoWorkspaceConfig;

    let integration = CargoWorkspace;
    if !is_enabled(&integration, ctx.config) {
        return Ok(Vec::new());
    }

    let cfg: CargoWorkspaceConfig = ctx.config.settings()?;
    // Reuse the integration's enablement predicate for "does this workspace
    // have cargo work at all?" so the scan silently no-ops in workspaces
    // without any Rust members.
    let members = CargoWorkspace::scan_members(ctx, &cfg)?;
    if members.is_empty() {
        return Ok(Vec::new());
    }

    let mut violations = Vec::new();
    for (crate_name, occurrences) in CargoWorkspace::scan_version_skew(ctx.workspace_root, &members)
    {
        violations.push(CheckViolation::CargoVersionSkew {
            crate_name,
            occurrences,
        });
    }
    for rec in CargoWorkspace::scan_patch_shadowing(ctx.workspace_root, &members) {
        violations.push(CheckViolation::CargoPatchShadowing {
            weave_config: rec.weave_config,
            member_config: rec.member_config,
            registry: rec.registry,
            crate_name: rec.crate_name,
        });
    }
    Ok(violations)
}

/// Scan all repos in `projects` for provenance violations.
///
/// Checks performed per repo on disk:
///
/// 1. **`origin-url-mismatch`** — the clone's `origin` remote URL differs
///    from the URL recorded in the manifest. Warning severity; report-only.
///    Reference-role repos may intentionally diverge (see note in violation
///    message).
///
/// 2. **`lock-sha-unreachable`** — a SHA pinned in `rwv.lock` is absent
///    from the local object store (`git cat-file -e <sha>^{commit}`). Error
///    severity; report-only. Remediation is a fetch from the remote, not a
///    sync.
///
/// Only repos that exist on disk are checked. For `lock-sha-unreachable`
/// the raw lock file is used (before `resolve_versions`) so that SHAs that
/// fail to resolve are the ones we test for reachability — if the lock SHA
/// is a tag or branch name it is resolved first; if that fails, the SHA is
/// tested verbatim.
pub fn scan_provenance(workspace_dir: &Path, projects: &[Project]) -> Vec<CheckViolation> {
    use crate::manifest::clone_urls_equivalent;
    use crate::vcs::Vcs;

    let git = crate::git::GitVcs;
    let mut violations = Vec::new();

    for project in projects {
        // --- origin-url-mismatch ---
        for (repo_path, entry) in project.manifest.iter_entries() {
            let repo_abs = workspace_dir.join(repo_path.as_path());
            if !repo_abs.is_dir() {
                continue;
            }

            let manifest_url = entry.url.to_string();
            let actual_url = match git.remote_url(&repo_abs, "origin") {
                Ok(Some(u)) => u,
                Ok(None) => continue, // no `origin` remote — not this check's concern
                Err(_) => continue,   // can't read remote — skip silently
            };

            if !clone_urls_equivalent(&manifest_url, &actual_url) {
                violations.push(CheckViolation::Provenance {
                    project: project.name.clone(),
                    repo: repo_path.clone(),
                    sub_kind: ProvenanceKind::OriginUrlMismatch {
                        manifest_url,
                        actual_url,
                        is_reference_role: entry.role == crate::manifest::Role::Reference,
                    },
                });
            }
        }

        // --- lock-sha-unreachable ---
        let raw_lock = match project.lock.as_ref() {
            Some(l) => l,
            None => continue,
        };

        for (repo_path, lock_entry) in raw_lock.iter_entries() {
            let repo_abs = workspace_dir.join(repo_path.as_path());
            if !repo_abs.is_dir() {
                continue;
            }

            // Try to resolve the raw version string to a canonical SHA.
            // If resolution succeeds, use the canonical SHA for the
            // reachability probe (handles tag/branch names correctly).
            // If resolution fails, the version string itself is likely
            // a SHA that git cannot find at all — test it directly so
            // we don't silently skip the reachability check in the
            // disconnected-clone / force-push scenario.
            let sha_to_test = match git.resolve_revision(&repo_abs, lock_entry.version.as_str()) {
                Ok(resolved) => resolved.as_str().to_owned(),
                Err(_) => lock_entry.version.as_str().to_owned(),
            };

            match git.commit_object_exists(&repo_abs, &sha_to_test) {
                Ok(true) => {} // present — all good
                Ok(false) => {
                    violations.push(CheckViolation::Provenance {
                        project: project.name.clone(),
                        repo: repo_path.clone(),
                        sub_kind: ProvenanceKind::LockShaUnreachable { sha: sha_to_test },
                    });
                }
                Err(_) => {} // can't probe — skip silently
            }
        }
    }

    violations
}

// ---------------------------------------------------------------------------
// Clone-topology scanning
// ---------------------------------------------------------------------------

/// Scan every `(workspace, repo)` pair under this weave's view and report
/// clone-topology violations of the I1/I2 invariants from
/// [`docs/explanation/joints/clone-topology.md`].
///
/// For each manifest repo `R`, the scanner gathers:
///   - the canonical slot at `<ws_root>/R` (call its canonical-store CAN);
///   - every workweave checkout `<workweave>/R` (call its store WW_i).
///
/// It then classifies each pair by comparing canonical-store paths
/// resolved through [`crate::vcs::Vcs::resolve_canonical_store`] (intent:
/// "which object DAG does this workspace belong to?"). The four sub-kinds
/// from the spec map to:
///
/// 1. **`weave-clone-is-worktree`** — CAN exists, but its canonical store is
///    not `<ws_root>/R/.git` (the slot is itself a linked worktree of some
///    other clone). Full inversion: the canonical has migrated out of the
///    manifest slot.
/// 2. **`standalone-in-workweave`** — WW_i is a full clone (its canonical
///    store sits under itself in `.workweaves/`). Other workweaves' checkouts
///    are typically linked into it — the inverted-primary shape.
/// 3. **`disconnected-weave-clone`** — CAN sits at `<ws_root>/R` correctly
///    (its store is under itself), but at least one workweave's WW_i resolves
///    to a different store: the weave clone publishes an object DAG nobody
///    syncs to, push/pull is silently asymmetric.
/// 4. **`wrong-parent-worktree`** — WW_i is a linked worktree whose canonical
///    store is not CAN's store (and is not WW_i's own store either — that
///    case is `standalone-in-workweave`). Cross-DAG merged-checks are silent.
///
/// All four are report-only: repair is an object-store migration. The
/// scanner is read-only.
///
/// A workspace that the VCS doesn't recognize as a repo at all is skipped
/// (not a topology violation — it might just be a manifest entry that
/// hasn't been materialized yet).
///
/// Symlink/trailing-slash differences are absorbed by canonicalizing both
/// sides before equality.
pub fn scan_clone_topology(ws_root: &Path, repo_paths: &BTreeSet<RepoPath>) -> Vec<CheckViolation> {
    use crate::git::GitVcs;
    use crate::vcs::Vcs;
    use crate::workweave::{classify_checkout, CheckoutKind};

    let mut violations = Vec::new();
    if repo_paths.is_empty() {
        return violations;
    }
    let git = GitVcs;

    // Collect every workweave under this weave once; we iterate per-repo
    // inside the loop.
    let workweaves = crate::workweave::list_workweave_dirs(ws_root);

    for repo in repo_paths {
        let canonical_slot = ws_root.join(repo.as_path());
        let canonical_store_raw = git.resolve_canonical_store(&canonical_slot);

        // Expected canonical store path: `<canonical_slot>/.git`. Compare via
        // canonicalize to absorb any trailing-slash / symlink differences.
        let expected_store_canon = canonical_slot
            .join(".git")
            .canonicalize()
            .unwrap_or_else(|_| canonical_slot.join(".git"));

        let canonical_store_canon: Option<PathBuf> = canonical_store_raw
            .as_ref()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));

        // Sub-kind: weave-clone-is-worktree.
        //
        // The canonical slot is a workspace but its canonical store doesn't
        // live underneath. `None` here means the slot is just absent or
        // un-materialized — not a topology violation; skip.
        if let Some(ref canonical_store_canon_pb) = canonical_store_canon {
            if canonical_store_canon_pb != &expected_store_canon {
                violations.push(CheckViolation::CloneTopology {
                    workspace_path: canonical_slot.clone(),
                    repo: repo.clone(),
                    sub_kind: CloneTopologyKind::WeaveCloneIsWorktree {
                        actual_store_path: canonical_store_raw
                            .clone()
                            .unwrap_or_else(|| canonical_store_canon_pb.clone()),
                    },
                });
                // Don't also classify it as disconnected — the canonical is
                // not even an independent store; the diagnosis is "the slot
                // is a worktree of something else".
            }
        }

        // Walk every workweave checkout of this repo and classify it.
        //
        // While walking, collect a representative store path for the
        // workweaves so we can compare against the canonical store and
        // surface `disconnected-weave-clone` when the canonical sits on its
        // own DAG that nobody syncs to.
        let mut representative_ww_store: Option<PathBuf> = None;
        for (_ww_name, ww_dir) in &workweaves {
            let ww_checkout = ww_dir.join(repo.as_path());

            // A symlinked reference checkout ([`CheckoutKind::ReferenceAlias`])
            // is the canonical store viewed through a symlink, not a second
            // store: it *upholds* I1 by identity. Skip it before any topology
            // sub-check.
            //
            // `git rev-parse --git-common-dir` follows the symlink and resolves
            // to `<weave>/<repo_path>/.git`, so `ww_self_store_canon` (also
            // resolved through the link) would equal it and fire a false
            // `StandaloneInWorkweave` (the inverted-primary shape). Excluding
            // the alias here — and only the alias — leaves genuine standalone
            // detection intact: a *real* standalone store inside a workweave is
            // a real directory, not a symlink, so it classifies as
            // [`CheckoutKind::Worktree`] and still flows through the
            // `StandaloneInWorkweave` check below.
            if classify_checkout(&ww_checkout) == CheckoutKind::ReferenceAlias {
                continue;
            }

            let ww_store_raw = match git.resolve_canonical_store(&ww_checkout) {
                Some(p) => p,
                None => continue, // not a workspace there; skip silently
            };
            let ww_store_canon = ww_store_raw
                .canonicalize()
                .unwrap_or_else(|_| ww_store_raw.clone());
            let ww_self_store_canon = ww_checkout
                .join(".git")
                .canonicalize()
                .unwrap_or_else(|_| ww_checkout.join(".git"));

            // Capture a representative for the disconnected-weave-clone
            // diagnosis below. Prefer a store that disagrees with the
            // canonical (it's the witness for the disconnection) over one
            // that agrees.
            let agrees_with_canonical = canonical_store_canon
                .as_ref()
                .map(|c| *c == ww_store_canon)
                .unwrap_or(false);
            if representative_ww_store.is_none() || !agrees_with_canonical {
                representative_ww_store = Some(ww_store_raw.clone());
            }

            // Sub-kind: standalone-in-workweave.
            //
            // The workweave checkout is itself a full clone (its canonical
            // store sits at `<workweave>/<repo>/.git`). This is the
            // inverted-primary shape: the canonical has migrated into one
            // workweave.
            if ww_store_canon == ww_self_store_canon {
                violations.push(CheckViolation::CloneTopology {
                    workspace_path: ww_checkout.clone(),
                    repo: repo.clone(),
                    sub_kind: CloneTopologyKind::StandaloneInWorkweave {
                        store_path: ww_store_raw.clone(),
                    },
                });
                // Don't also flag as wrong-parent — the diagnosis is the
                // sharper one (this *is* a standalone store, not a worktree
                // linked to the wrong parent).
                continue;
            }

            // Sub-kind: wrong-parent-worktree.
            //
            // The workweave checkout is a linked worktree of some store,
            // but that store is not the weave canonical. Cross-DAG silent
            // failures incoming.
            //
            // We compare against the *expected* canonical-store path
            // (`<ws_root>/<repo>/.git`), not the canonical's actual
            // resolved store: when the canonical itself is broken
            // (weave-clone-is-worktree), the right thing to say about the
            // workweave is still "it should have been linked to the
            // canonical slot, but it's linked elsewhere". Two violations
            // surface, both pointing at the topology problem.
            if ww_store_canon != expected_store_canon {
                violations.push(CheckViolation::CloneTopology {
                    workspace_path: ww_checkout.clone(),
                    repo: repo.clone(),
                    sub_kind: CloneTopologyKind::WrongParentWorktree {
                        expected_store_path: canonical_slot.join(".git"),
                        actual_store_path: ww_store_raw.clone(),
                    },
                });
            }
        }

        // Sub-kind: disconnected-weave-clone.
        //
        // Reported only when the canonical is an apparently-healthy full
        // clone (its store sits at `<ws_root>/<repo>/.git`), but a
        // workweave checkout resolves to a different store. The canonical
        // is publishing an isolated DAG nobody syncs to. Skip when there
        // are no workweave checkouts at all (a lone canonical is a healthy
        // base case).
        if let Some(ref canonical_store_canon_pb) = canonical_store_canon {
            if canonical_store_canon_pb == &expected_store_canon {
                if let Some(rep) = representative_ww_store {
                    let rep_canon = rep.canonicalize().unwrap_or_else(|_| rep.clone());
                    if rep_canon != expected_store_canon {
                        violations.push(CheckViolation::CloneTopology {
                            workspace_path: canonical_slot.clone(),
                            repo: repo.clone(),
                            sub_kind: CloneTopologyKind::DisconnectedWeaveClone {
                                weave_store_path: canonical_store_raw
                                    .clone()
                                    .unwrap_or_else(|| expected_store_canon.clone()),
                                other_store_path: rep,
                            },
                        });
                    }
                }
            }
        }
    }

    violations
}

// ---------------------------------------------------------------------------
// Branch-discipline scanning
// ---------------------------------------------------------------------------
//
// Three checks, one symbolic-ref read per checkout plus one branch listing
// per canonical. Together they enforce the I3 invariant from
// `docs/explanation/joints/clone-topology.md` — every workweave repo
// checkout sits on a `<project>--<workweave>/<segment>` ephemeral branch
// owned by exactly that workweave; canonicals sit on a non-ephemeral
// branch; and stale ephemeral branches left in canonicals by crashed
// deletes are surfaced under the safe/live doctrine from
// `docs/explanation/joints/shared-refs-drift.md`.
//
// VCS seam: the scanner consumes the `Vcs` trait — `current_ref`,
// `list_local_branches`, `head_revision`, `resolve_revision`, and
// `is_ancestor` — without any git-specific code. See
// `docs/explanation/joints/vcs-as-seam.md`.

/// Strip the canonical `refs/heads/` prefix returned by
/// [`Vcs::list_local_branches`](crate::vcs::Vcs::list_local_branches) so
/// the bare branch name can be compared against `<project>--<name>/...`
/// patterns.
fn bare_branch_name(branch: &crate::vcs::RefName) -> String {
    branch
        .as_str()
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.as_str())
        .to_string()
}

/// Split a candidate ephemeral branch name into (project, workweave_name,
/// segment), returning `None` when the name doesn't match the
/// `<project>--<workweave>/<segment>` shape.
fn parse_ephemeral_branch_name(branch: &str) -> Option<(&str, &str, &str)> {
    let (lhs, segment) = branch.split_once('/')?;
    if segment.is_empty() {
        return None;
    }
    let (project, workweave) = lhs.split_once("--")?;
    if project.is_empty() || workweave.is_empty() {
        return None;
    }
    Some((project, workweave, segment))
}

/// Build the set of workweave directory basenames that currently exist
/// under `<ws_root>/.workweaves/`. Used by (c) to decide whether a
/// `<project>--<name>/...` branch in a canonical is stale.
fn existing_workweave_dir_names(ws_root: &Path) -> std::collections::HashSet<String> {
    let parent = crate::workweave::workweave_parent_pub(ws_root);
    let mut out = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                out.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    out
}

/// Read each repo checkout's HEAD ref via the `Vcs` trait. Returns
/// `Ok(Some(branch))` when on a named branch, `Ok(None)` when detached.
fn read_current_branch(
    vcs: &dyn crate::vcs::Vcs,
    repo: &Path,
) -> Result<Option<crate::vcs::RefName>, crate::vcs::VcsError> {
    vcs.current_ref(repo)
}

/// Scan a workweave's repo checkouts for (a) workweave-branch violations.
///
/// For each git repo under `workweave_dir`, the HEAD's symbolic-ref must
/// match the prefix `<project>--<workweave>/`. Three sub-kinds catch the
/// failure modes:
///
///   * [`BranchDisciplineKind::Detached`] — HEAD points at a SHA, not a
///     branch (e.g. an explicit `git checkout <sha>`).
///   * [`BranchDisciplineKind::SharedBranch`] — HEAD is on a non-ephemeral
///     branch (e.g. `main`); covers the bare-main-in-workweave case from
///     the spec's acceptance criteria.
///   * [`BranchDisciplineKind::ForeignEphemeral`] — HEAD is on an
///     ephemeral branch belonging to a *different* workweave (e.g. the
///     directory was rsync'd from another workweave whose branches it
///     kept).
///
/// The expected prefix is `<project>--<workweave>` (without the trailing
/// `/`); a branch matching `<prefix>/<segment>` for any non-empty
/// `<segment>` is treated as the owned ephemeral namespace.
fn scan_workweave_repo_branches(
    vcs: &dyn crate::vcs::Vcs,
    workweave_dir: &Path,
    project_name: &str,
    workweave_name: &str,
    out: &mut Vec<CheckViolation>,
) {
    use crate::workweave::{classify_checkout, CheckoutKind};

    let expected_prefix = format!("{project_name}--{workweave_name}");
    let registries = crate::registry::builtin_registries();
    let repos = crate::workspace::scan_repos_on_disk(workweave_dir, &registries, vcs);
    for repo in repos {
        let abs = workweave_dir.join(repo.as_path());

        // `scan_repos_on_disk` discovers entries with `is_dir()`, which
        // *follows* symlinks — so a symlinked reference checkout
        // ([`CheckoutKind::ReferenceAlias`]) is surfaced as a repo. It is the
        // canonical store viewed through a symlink, sitting on the canonical's
        // shared non-ephemeral branch (e.g. `main`) by design; the I3
        // branch-discipline scan would mis-read that as a `SharedBranch`
        // violation. Skip the alias before the branch check.
        //
        // This excludes *only* the symlink: a `reference` repo materialized via
        // `--worktree-references` is a real worktree on its own ephemeral
        // branch — it classifies as [`CheckoutKind::Worktree`] and flows
        // through the I3 check unchanged.
        if classify_checkout(&abs) == CheckoutKind::ReferenceAlias {
            continue;
        }

        match read_current_branch(vcs, &abs) {
            Ok(Some(branch)) => {
                let bare = branch.as_str();
                let expected_full_prefix = format!("{expected_prefix}/");
                if bare.starts_with(&expected_full_prefix)
                    && bare.len() > expected_full_prefix.len()
                {
                    continue; // healthy
                }
                // Tease out shared vs foreign: a branch with the
                // `<other>--<other>/...` shape names *some* workweave but
                // not the right one; anything else (including plain
                // `main` / `master` / a feature branch with no `--`) is
                // a shared-branch finding.
                let sub_kind = if parse_ephemeral_branch_name(bare).is_some() {
                    BranchDisciplineKind::ForeignEphemeral {
                        actual_branch: bare.to_string(),
                        expected_prefix: expected_prefix.clone(),
                    }
                } else {
                    BranchDisciplineKind::SharedBranch {
                        actual_branch: bare.to_string(),
                        expected_prefix: expected_prefix.clone(),
                    }
                };
                out.push(CheckViolation::BranchDiscipline {
                    repo_path: abs,
                    sub_kind,
                });
            }
            Ok(None) => {
                // Detached HEAD.
                out.push(CheckViolation::BranchDiscipline {
                    repo_path: abs,
                    sub_kind: BranchDisciplineKind::Detached,
                });
            }
            Err(_) => {
                // Treat read failures as best-effort silence (matches
                // existing doctor patterns for transient git errors).
            }
        }
    }
}

/// Scan every canonical repo under `ws_root` for (b) ephemeral-at-primary
/// and (c) stale-ephemeral-branches.
///
/// (b): the canonical must not be checked out on any `<project>--<name>/...`
/// branch — the inverse of (a). A canonical on such a branch indicates the
/// operator switched the canonical to a workweave's branch, or a workweave
/// directory was deleted while the canonical was still holding its
/// ephemeral branch.
///
/// (c): every ephemeral-named branch in the canonical whose workweave
/// `<name>` no longer exists on disk is reported. The safe/live split
/// (see [`BranchDisciplineKind`]) consults
/// [`Vcs::is_ancestor`](crate::vcs::Vcs::is_ancestor) — a branch tip that
/// is an ancestor of the primary tracking branch's tip carries no unique
/// work and is safe class; anything else is live class.
fn scan_canonical_branches(
    vcs: &dyn crate::vcs::Vcs,
    ws_root: &Path,
    out: &mut Vec<CheckViolation>,
) {
    let existing_workweaves = existing_workweave_dir_names(ws_root);
    let registries = crate::registry::builtin_registries();
    let repos = crate::workspace::scan_repos_on_disk(ws_root, &registries, vcs);

    for repo in repos {
        let abs = ws_root.join(repo.as_path());

        // (b) ephemeral-at-primary.
        if let Ok(Some(branch)) = read_current_branch(vcs, &abs) {
            if parse_ephemeral_branch_name(branch.as_str()).is_some() {
                out.push(CheckViolation::BranchDiscipline {
                    repo_path: abs.clone(),
                    sub_kind: BranchDisciplineKind::EphemeralAtPrimary {
                        actual_branch: branch.as_str().to_string(),
                    },
                });
            }
        }

        // (c) stale-ephemeral-branches. One branch listing per canonical.
        let branches = match vcs.list_local_branches(&abs) {
            Ok(b) => b,
            Err(_) => continue,
        };

        // Cache the primary tip per repo so per-branch safe/live checks
        // share one `head_revision` call.
        let primary_tip = vcs.head_revision(&abs).ok();

        for branch_ref in &branches {
            let bare = bare_branch_name(branch_ref);
            let (_project, workweave_name, _segment) = match parse_ephemeral_branch_name(&bare) {
                Some(parts) => parts,
                None => continue,
            };
            // The workweave directory basename is `<project>--<workweave>`
            // (mirrors `workspace::weave_dir_name`). If that directory
            // still exists, the branch is owned and healthy.
            let dir_basename = bare
                .split('/')
                .next()
                .expect("split('/') on non-empty string yields at least one element");
            if existing_workweaves.contains(dir_basename) {
                continue;
            }

            // Stale — classify safe vs live.
            let tip = match vcs.resolve_revision(&abs, &bare) {
                Ok(rev) => rev,
                Err(_) => continue, // can't classify; skip rather than mis-report
            };

            let safe = match &primary_tip {
                Some(primary) => vcs.is_ancestor(&abs, &tip, primary).unwrap_or(false),
                // No primary tip readable (empty repo / corruption) — be
                // conservative and call it live so `--fix` won't touch it.
                None => false,
            };

            let sub_kind = if safe {
                BranchDisciplineKind::StaleEphemeralBranchSafe {
                    branch: bare.clone(),
                    workweave_name: workweave_name.to_string(),
                }
            } else {
                BranchDisciplineKind::StaleEphemeralBranchLive {
                    branch: bare.clone(),
                    workweave_name: workweave_name.to_string(),
                    tip_sha: tip.as_str().to_string(),
                }
            };
            out.push(CheckViolation::BranchDiscipline {
                repo_path: abs.clone(),
                sub_kind,
            });
        }
    }
}

/// Scan every workweave repo checkout for uninitialized submodules.
///
/// For each workweave repo whose worktree has a `.gitmodules` file, this
/// function checks whether any of the listed submodule paths are empty
/// directories. An empty submodule directory indicates that
/// `git submodule update --init` has never run — the commit records the
/// submodule but the content is absent.
///
/// **Cost**: one `Path::exists` call per repo per workweave when `.gitmodules`
/// is absent (the common case). The `.gitmodules` parse + directory stat only
/// runs when the file exists. No network I/O.
///
/// **Scope**: workweave checkouts only. Primary weave clones are expected to
/// have submodules initialized at clone time; workweave worktrees are created
/// by `git worktree add`, which does NOT re-run submodule init.
pub fn scan_uninitialized_submodules_in_workweaves(
    ws_root: &Path,
    projects: &[crate::manifest::Project],
) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    for (workweave_name_str, workweave_dir) in crate::workweave::list_workweave_dirs(ws_root) {
        let workweave_name = WorkweaveName::new(workweave_name_str);
        for project in projects {
            for (repo_path, _entry) in project.manifest.iter_entries() {
                let worktree = workweave_dir.join(repo_path.as_path());
                // Skip reference aliases (symlinks): they share the canonical
                // store which owns submodule init. Only real worktrees get
                // a fresh worktree that might miss submodule init.
                if crate::workweave::classify_checkout(&worktree)
                    == crate::workweave::CheckoutKind::ReferenceAlias
                {
                    continue;
                }
                if !worktree.is_dir() {
                    continue;
                }
                let empty_paths = crate::workweave::scan_uninitialized_submodules(&worktree);
                if !empty_paths.is_empty() {
                    violations.push(CheckViolation::UninitializedSubmodule {
                        workweave: workweave_name.clone(),
                        repo: repo_path.clone(),
                        empty_paths,
                    });
                }
            }
        }
    }

    violations
}

/// Scan branch-discipline (workweave-branch + ephemeral-at-primary +
/// stale-ephemeral-branches) across the workspace rooted at `ws_root`
/// (which must be the primary).
///
/// One symbolic-ref read per workweave checkout plus one branch listing
/// per canonical. The check is VCS-neutral: it consumes only the [`Vcs`]
/// trait surface and never spells git plumbing.
///
/// See:
///   * `docs/explanation/joints/clone-topology.md` (I3 — branch ownership).
///   * `docs/explanation/joints/shared-refs-drift.md` (safe/live doctrine,
///     applied here to refs instead of blobs).
///
/// [`Vcs`]: crate::vcs::Vcs
pub fn scan_branch_discipline(ws_root: &Path, vcs: &dyn crate::vcs::Vcs) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    // (a) workweave-branch: per workweave under .workweaves/, per repo
    // checkout, validate the HEAD symbolic-ref prefix.
    for (workweave_name, workweave_dir) in crate::workweave::list_workweave_dirs(ws_root) {
        // Resolve the project name from the workweave marker — the marker
        // is authoritative (`workspace::WorkweaveMarker::read`) and is
        // already required to exist by the tree-integrity scanner.
        let marker = match crate::workspace::WorkweaveMarker::read(&workweave_dir) {
            Ok(Some(m)) => m,
            // Missing or unparseable marker → tree-integrity scan owns the
            // reporting; do not pile on a noisy branch-discipline finding
            // for the same directory.
            _ => continue,
        };
        scan_workweave_repo_branches(
            vcs,
            &workweave_dir,
            marker.project.as_str(),
            &workweave_name,
            &mut violations,
        );
    }

    // (b) + (c) — scan canonical clones under the primary.
    scan_canonical_branches(vcs, ws_root, &mut violations);

    violations
}

// State-hygiene scanning (stale worktree registrations, stale .rwv-op,
// orphaned savepoints).
// ---------------------------------------------------------------------------

/// One repo to scan for state-hygiene violations.
///
/// The scanner is driven by an explicit list rather than re-deriving the set
/// from the manifest: callers (`run_check`, `collect_doctor_violations`)
/// already build a deduped scan list for drift detection, and reusing the
/// same `(workweave, abs, repo_path)` shape here keeps the input contract
/// uniform across check kinds.
pub struct StateHygieneScanTarget {
    /// Workweave name when the repo lives inside a workweave; `None` for
    /// repos in the primary weave.
    pub workweave: Option<WorkweaveName>,
    /// Absolute path to the repo on disk.
    pub abs: PathBuf,
    /// Manifest-relative repo path (used to build the violation's `path`).
    pub repo: RepoPath,
}

/// One workspace directory that may carry a `.rwv-op` op-state file.
pub struct StateHygieneOpStateTarget {
    /// Absolute path to the workspace dir (active path of a workspace —
    /// primary weave root or a workweave directory).
    pub workspace_dir: PathBuf,
}

/// Scan the supplied repos and workspace dirs for the three state-hygiene
/// check kinds — stale worktree registrations, stale `.rwv-op` files,
/// and orphaned savepoints. All three are workspace-scope hygiene
/// findings; none of them depend on the manifest or per-project lock,
/// so they share a single scanner entry point.
///
/// **Classification policy:**
///
/// - **stale-worktree-registration**: produced by
///   [`Vcs::list_stale_worktree_registrations`]. `--fix` (in `run_check`)
///   runs [`Vcs::worktree_prune`]; this scanner only reports.
/// - **stale-op-state**: an in-tree `.rwv-op` file (any one). Reported
///   with its `started_at` field intact. **Never auto-fixed** — another
///   terminal may be mid-conflict-resolution and rwv has no daemon to
///   know.
/// - **orphaned-savepoint**: a `refs/rwv/pre-op/<op-id>` ref whose
///   `op_id` is not present in any live `.rwv-op` file inside
///   `op_state_targets`. Classified as
///   [`OrphanedSavepointKind::Redundant`] when the savepoint tip is an
///   ancestor of the repo's current ref (the underlying commits remain
///   anchored by the live branch, so dropping the savepoint loses no
///   objects), and [`OrphanedSavepointKind::Live`] otherwise. Only
///   `Redundant` savepoints are `--fix`-eligible — dropping a `Live`
///   savepoint would discard the last pointer to commits not held by
///   any other ref, same rationale that put reflog on the FORBIDDEN
///   tripwire list.
///
/// Returns the violations in the canonical order produced by iterating
/// `repos` and then `op_state_targets`; the caller is responsible for
/// any further sorting.
pub fn scan_state_hygiene(
    vcs: &dyn crate::vcs::Vcs,
    repos: &[StateHygieneScanTarget],
    op_state_targets: &[StateHygieneOpStateTarget],
) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    // Phase 1: collect the live op-ids from every `.rwv-op` file under the
    // workspace tree. We need this set before classifying savepoints below;
    // any savepoint whose op-id is in this set is in-use (the matching sync
    // is in flight) and must not be reported as orphaned.
    let mut live_op_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for target in op_state_targets {
        // Absent or unparseable file → nothing to do here. An unparseable
        // file is surfaced by `op_state::read_owner`'s caller in sync paths;
        // the doctor's job for the `.rwv-op` line is just to report
        // presence, not to debug the YAML.
        if let Ok(Some(state)) = crate::op_state::read_owner(&target.workspace_dir) {
            violations.push(CheckViolation::StaleOpState {
                workspace_dir: target.workspace_dir.clone(),
                started_at: state.started_at.clone(),
            });
            live_op_ids.insert(state.id);
        }
        // Dead-lease check: a workspace holding an `.rwv-op-lease` whose
        // recorded owner has no matching `.rwv-op` (or has one with a
        // different op id) is structurally broken. Classified by pointer
        // resolution alone — no wall-clock input. Safe to auto-fix by
        // removing the lease file (the paired owner record is either gone
        // or belongs to a different op, so removing the lease can't clobber
        // an in-flight op).
        if let Ok(Some(dead)) = crate::op_state::detect_dead_lease(&target.workspace_dir) {
            let sub_kind = match dead.reason {
                crate::op_state::DeadLeaseReason::OwnerRecordAbsent => {
                    DeadOpLeaseKind::OwnerRecordAbsent
                }
                crate::op_state::DeadLeaseReason::OwnerOpIdMismatch { owner_op_id } => {
                    DeadOpLeaseKind::OwnerOpIdMismatch { owner_op_id }
                }
            };
            violations.push(CheckViolation::DeadOpLease {
                workspace_dir: dead.workspace_dir,
                op_id: dead.op_id,
                recorded_owner: dead.recorded_owner,
                sub_kind,
                created_at: dead.created_at,
            });
        }
    }

    // Phase 2: per-repo checks (stale worktree registrations + orphaned
    // savepoints).
    for repo in repos {
        // Skip repos that are not vcs-managed (the manifest may list a
        // path that hasn't been cloned yet).
        if !vcs.is_repo(&repo.abs) {
            continue;
        }

        // Stale worktree registrations.
        if let Ok(stale_paths) = vcs.list_stale_worktree_registrations(&repo.abs) {
            for missing_path in stale_paths {
                violations.push(CheckViolation::StaleWorktreeRegistration {
                    workweave: repo.workweave.clone(),
                    repo: repo.repo.clone(),
                    missing_path,
                });
            }
        }

        // Orphaned savepoints. Resolve current HEAD up front so we can
        // classify each ref; if HEAD itself is unreadable, we report
        // every savepoint as Live (conservative — we cannot prove
        // reachability, so we keep the ref).
        let head = vcs.head_revision(&repo.abs).ok();
        let op_ids = match vcs.list_savepoint_op_ids(&repo.abs) {
            Ok(ids) => ids,
            Err(_) => continue, // can't enumerate → leave repo alone
        };
        for op_id in op_ids {
            // Skip savepoints whose op-id matches an in-flight `.rwv-op`.
            // The owning sync may still need to roll back.
            if live_op_ids.contains(&op_id) {
                continue;
            }
            let sub_kind = match (&head, vcs.resolve_savepoint(&repo.abs, &op_id)) {
                (Some(head_rev), Some(sp_rev)) => {
                    // `is_ancestor(sp, head)` is non-strict: equal revisions
                    // return true, which is what we want — a savepoint
                    // pointing at the same commit as HEAD is trivially
                    // redundant.
                    match vcs.is_ancestor(&repo.abs, &sp_rev, head_rev) {
                        Ok(true) => OrphanedSavepointKind::Redundant,
                        // Not reachable from HEAD, or git couldn't decide:
                        // assume Live to stay on the safe side. The
                        // "couldn't decide" branch is conservative — we
                        // don't have proof of reachability.
                        Ok(false) | Err(_) => OrphanedSavepointKind::Live,
                    }
                }
                // No HEAD or no savepoint SHA → conservative: keep the ref.
                _ => OrphanedSavepointKind::Live,
            };
            violations.push(CheckViolation::OrphanedSavepoint {
                workweave: repo.workweave.clone(),
                repo: repo.repo.clone(),
                op_id,
                sub_kind,
            });
        }
    }

    violations
}

/// Return `true` if `violation` is a [`CheckViolation::BranchDiscipline`]
/// that belongs to `active_project`.
///
/// Used to scope branch-discipline findings (and the corresponding `--fix`
/// deletions) to the active project when `scope_all` is `false`.
///
/// Two path shapes are handled:
///
/// - **Workweave checkout** (sub-kinds a: `SharedBranch`, `ForeignEphemeral`,
///   `Detached`): `repo_path` lives under the workweave parent directory
///   (`<ws_root>/../.workweaves/<project>--<ww_name>/`).  The project is
///   the `<project>` prefix extracted from the workweave directory basename
///   via [`crate::workspace::parse_weave_dir_name`].
///
/// - **Canonical clone** (sub-kinds b/c: `EphemeralAtPrimary`,
///   `StaleEphemeralBranchSafe`, `StaleEphemeralBranchLive`): `repo_path`
///   lives directly under `ws_root`.  The manifest-relative path is
///   derived by stripping `ws_root`, normalised to forward slashes, and
///   looked up in `known_repos`.  When `known_repos` was built from only
///   the active project's manifest (the default when `!scope_all`), a hit
///   means the active project owns this repo.
///
/// Returns `true` (include) when:
/// * the violation is not `BranchDiscipline` (shouldn't happen in
///   callers, but be safe),
/// * the project matches `active_project`, or
/// * the repo is in `known_repos`.
///
/// Returns `false` (exclude / out of scope) otherwise.
fn branch_discipline_in_scope(
    violation: &CheckViolation,
    ws_root: &Path,
    active_project: &str,
    known_repos: &BTreeSet<RepoPath>,
) -> bool {
    let repo_path = match violation {
        CheckViolation::BranchDiscipline { repo_path, .. } => repo_path,
        _ => return true, // non-BD violations: caller handles them separately
    };

    // Determine the workweave parent (mirrors `workweave_parent_pub`).
    let ww_parent = crate::workweave::workweave_parent_pub(ws_root);

    if let Ok(rel_from_ww_parent) = repo_path.strip_prefix(&ww_parent) {
        // (a) path: under .workweaves/<project>--<ww_name>/...
        // Extract the first path component — that is the workweave dir name.
        let ww_dir_name = rel_from_ww_parent
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned());
        if let Some(dir_name) = ww_dir_name {
            if let Some((proj, _)) = crate::workspace::parse_weave_dir_name(&dir_name) {
                return proj == active_project;
            }
        }
        // Can't parse the workweave dir name → conservative: exclude.
        false
    } else if let Ok(rel_from_ws) = repo_path.strip_prefix(ws_root) {
        // (b)/(c) path: under ws_root.
        // Convert to forward-slash string and look up in known_repos.
        let rel_str = rel_from_ws
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if let Ok(rp) = RepoPath::new(rel_str) {
            return known_repos.contains(&rp);
        }
        false
    } else {
        // Path is neither under the workweave parent nor under ws_root.
        // Shouldn't happen in practice; conservative: exclude.
        false
    }
}

/// Apply the `rwv doctor --fix` deletion for safe-class stale ephemeral
/// branches in canonicals.
///
/// Idempotent and information-preserving: only branches that
/// [`scan_branch_discipline`] classified as
/// [`BranchDisciplineKind::StaleEphemeralBranchSafe`] are deleted. The
/// classification is verified again before each delete — the safe class
/// requires `is_ancestor(tip, primary_tip) = true`, so deletion loses no
/// commits that aren't already reachable from the primary. Live-class
/// branches are never touched: the operator must recover or delete by
/// hand.
///
/// When `active_project` is `Some(name)`, only safe-class branches that
/// belong to that project are deleted (same scoping as `run_check` without
/// `--all`). Pass `None` to apply the deletion weave-wide (matches the
/// `--all` path).
///
/// Returns `(deleted, errors)` so the caller can render `[fixed]` lines
/// for successful deletions and surface failures as issues.
pub fn fix_stale_ephemeral_branches(
    ws_root: &Path,
    vcs: &dyn crate::vcs::Vcs,
    active_project: Option<&str>,
    known_repos: &BTreeSet<RepoPath>,
) -> (Vec<(PathBuf, String)>, Vec<String>) {
    use crate::vcs::RefName;
    let mut deleted = Vec::new();
    let mut errors = Vec::new();

    // Re-scan so each delete sees the latest disk state and re-verifies
    // the safe-class precondition. `--fix` is meant to be idempotent: a
    // second invocation finds no safe-class violations to act on.
    for violation in scan_branch_discipline(ws_root, vcs) {
        // Project-scope filter: only act on findings that belong to the
        // active project (or all when active_project is None).
        if let Some(ap) = active_project {
            if !branch_discipline_in_scope(&violation, ws_root, ap, known_repos) {
                continue;
            }
        }
        let (repo_path, branch_name) = match violation {
            CheckViolation::BranchDiscipline {
                repo_path,
                sub_kind:
                    BranchDisciplineKind::StaleEphemeralBranchSafe {
                        branch,
                        workweave_name: _,
                    },
            } => (repo_path, branch),
            // Every other variant (including live-class stale branches and
            // the report-only (a)/(b) findings) is left untouched.
            _ => continue,
        };
        let branch_ref = RefName::new(branch_name.clone());
        match vcs.delete_branch(&repo_path, &branch_ref) {
            Ok(()) => deleted.push((repo_path, branch_name)),
            Err(e) => errors.push(format!(
                "failed to delete safe-class stale ephemeral branch `{}` in {}: {}",
                branch_name,
                repo_path.display(),
                e
            )),
        }
    }

    (deleted, errors)
}

/// Apply the `--fix` for a single state-hygiene violation.
///
/// The accepted `--fix` paths are deliberately narrow:
///
/// - [`CheckViolation::StaleWorktreeRegistration`] →
///   [`Vcs::worktree_prune`] in the registering repo. Information-
///   preserving by construction (the only state being dropped is a
///   pointer to a directory that already does not exist).
/// - [`CheckViolation::OrphanedSavepoint`] with
///   [`OrphanedSavepointKind::Redundant`] → [`Vcs::drop_savepoint`].
///   The ref tip is reachable from the current branch, so dropping the
///   ref does not unanchor any commits.
/// - [`CheckViolation::DeadOpLease`] → [`crate::op_state::fix_dead_lease`]
///   removes the dangling `.rwv-op-lease`. Safe by construction: the
///   classification proved the lease is paired with no live owner record
///   (either the owner file is gone or the owner is now on a different
///   op), so no in-flight op can be disrupted. Structural — no wall-clock
///   input.
///
/// All other variants (`StaleOpState`, `OrphanedSavepoint { Live, .. }`)
/// return `Ok(false)`: not auto-fixable, the caller should report only.
/// Returns `Ok(true)` when a fix was actually applied; `Ok(false)` when
/// the violation isn't in the `--fix` set; `Err` only when the
/// underlying VCS operation itself errored.
pub fn fix_state_hygiene(
    vcs: &dyn crate::vcs::Vcs,
    violation: &CheckViolation,
    repo_abs: &Path,
) -> anyhow::Result<bool> {
    match violation {
        CheckViolation::StaleWorktreeRegistration { .. } => {
            vcs.worktree_prune(repo_abs)
                .with_context(|| format!("worktree prune failed in {}", repo_abs.display()))?;
            Ok(true)
        }
        CheckViolation::OrphanedSavepoint {
            op_id,
            sub_kind: OrphanedSavepointKind::Redundant,
            ..
        } => {
            // drop_savepoint swallows ref-update errors by design — the
            // savepoint is purely a recovery aid.
            vcs.drop_savepoint(repo_abs, op_id);
            Ok(true)
        }
        CheckViolation::DeadOpLease { workspace_dir, .. } => {
            crate::op_state::fix_dead_lease(workspace_dir);
            Ok(true)
        }
        // Live orphaned savepoints, stale op-state, and every other variant
        // are not in the `--fix` set.
        _ => Ok(false),
    }
}

/// `$schema` URL embedded in `rwv doctor --json` output. Points at the
/// committed schema artifact in the main branch (Agent D regenerates this
/// file via `cargo run --bin generate-schemas` and CI fails on drift).
pub const DOCTOR_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/doctor.json";

/// Inputs for running workspace-wide checks.
pub struct CheckInput {
    /// All repos referenced by any project's `rwv.yaml`.
    pub known_repos: BTreeSet<RepoPath>,
    /// All git repos found on disk under registry directories.
    pub repos_on_disk: Vec<RepoPath>,
    /// Loaded projects.
    pub projects: Vec<Project>,
    /// Resolved HEAD revisions for repos on disk, keyed by RepoPath.
    pub head_revisions: BTreeMap<RepoPath, ResolvedRevisionId>,
    /// Resolved lock files keyed by project name. Built by the caller via
    /// [`crate::manifest::LockFile::resolve_versions`] before invoking
    /// [`find_violations`]; only projects whose lock could be resolved
    /// appear here. The split out of `Project.lock` (which stays raw)
    /// keeps the parse/resolve boundary explicit at the type level.
    pub resolved_locks: std::collections::HashMap<ProjectName, crate::manifest::ResolvedLockFile>,
    /// When `false`, the orphan-clone check is skipped. Orphan detection is
    /// inherently weave-wide (a repo is only "orphaned" if it belongs to *no*
    /// project), so running it in single-project mode would produce false
    /// positives for repos that belong to other projects. Set to `true` only
    /// when all projects have been loaded into `projects` (i.e. `--all` mode).
    pub check_orphans: bool,
}

/// Collect all convention violations from the check inputs.
///
/// This is a pure function: it takes data in, returns violations out.
/// Filesystem access (reading HEADs, scanning directories) happens
/// before this function is called.
pub fn find_violations(input: &CheckInput) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    // Orphaned clones: on disk but not in any project.
    // Only run when check_orphans is true (i.e. all projects are loaded).
    // In single-project mode, a repo absent from the active project may still
    // belong to another project — flagging it as orphaned would be a false positive.
    if input.check_orphans {
        for repo_path in &input.repos_on_disk {
            if !input.known_repos.contains(repo_path) {
                violations.push(CheckViolation::OrphanedClone {
                    path: repo_path.clone(),
                });
            }
        }
    }

    // Per-project checks
    for project in &input.projects {
        for (repo_path, entry) in project.manifest.iter_entries() {
            // Dangling reference: in manifest but not on disk.
            // Reference repos are allowed to be missing (e.g. after fetch --no-reference).
            if !input.repos_on_disk.contains(repo_path) && entry.role != Role::Reference {
                violations.push(CheckViolation::DanglingReference {
                    project: project.name.clone(),
                    repo: repo_path.clone(),
                });
            }
        }

        // Compare lock entries against resolved HEADs. The lock entries
        // are pulled from `input.resolved_locks` (built by the caller via
        // `LockFile::resolve_versions`), so equality is purely a
        // canonical-SHA comparison — the raw-vs-resolved confusion that
        // produced the historical B3/B6 bugs is now a compile-time
        // impossibility.
        if let Some(lock) = input.resolved_locks.get(&project.name) {
            for (repo_path, lock_entry) in lock.iter_entries() {
                if let Some(actual_rev) = input.head_revisions.get(repo_path) {
                    if &lock_entry.version != actual_rev {
                        violations.push(CheckViolation::StaleLock {
                            project: project.name.clone(),
                            repo: repo_path.clone(),
                            locked: lock_entry.version.clone(),
                            actual: actual_rev.clone(),
                        });
                    }
                }
            }
        }
    }

    violations
}

/// Convert check violations into the same `Issue` type that integrations use,
/// so all check results have a uniform shape.
pub fn violations_to_issues(violations: Vec<CheckViolation>) -> Vec<Issue> {
    violations
        .into_iter()
        .map(|v| {
            // safe_to_fix defaults to true; live-class branch-discipline
            // findings override to false so `doctor --fix` leaves them alone.
            let mut safe_to_fix = true;
            let (severity, message) = match v {
                CheckViolation::OrphanedClone { path } => (
                    crate::integration::Severity::Error,
                    format!(
                        "orphaned clone: {path} — not listed in any project's rwv.yaml; \
                         run `rwv add <url>` to register it, or remove the directory manually"
                    ),
                ),
                // `rwv fetch` (no SOURCE) re-clones missing manifest
                // members of the active project — the settled repair verb
                // for dangling references. See fetch::run_fetch_in_place.
                // `doctor --fix` intentionally does NOT auto-clone: network
                // side effects stay behind an explicit verb.
                CheckViolation::DanglingReference { project, repo } => (
                    crate::integration::Severity::Error,
                    format!(
                        "dangling reference in {project}: {repo} — \
                         listed in rwv.yaml but not cloned on disk; \
                         run `rwv fetch` from the workspace to re-materialize \
                         missing manifest members, then re-run `rwv doctor` to verify"
                    ),
                ),
                CheckViolation::MissingRole { project, repo } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "missing role in {project}: {repo} — \
                         add a `role: owned|dependency|reference` field to the \
                         rwv.yaml entry for this repo"
                    ),
                ),
                CheckViolation::StaleLock {
                    project,
                    repo,
                    locked,
                    actual,
                } => (
                    crate::integration::Severity::Error,
                    format!(
                        "stale lock in {project}: {repo} locked={locked} actual={actual}; \
                         run `rwv lock` to re-snapshot current HEAD SHAs"
                    ),
                ),
                CheckViolation::WorkweaveDrift {
                    workweave,
                    kind,
                    repo,
                } => {
                    let (kind_str, hint) = match kind {
                        DriftKind::Missing => (
                            "missing worktree",
                            "; run `rwv workweave <project> create --force` to \
                             recreate, or remove the repo from rwv.yaml",
                        ),
                        DriftKind::Extra => (
                            "extra worktree",
                            "; this worktree has no manifest entry — \
                             run `rwv add` to register it or remove it manually",
                        ),
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!("workweave drift in {workweave}: {kind_str} {repo}{hint}"),
                    )
                }
                CheckViolation::IndexDrift {
                    workweave,
                    repo,
                    kind,
                } => {
                    let location = match workweave {
                        Some(ww) => format!("{ww}/{repo}"),
                        None => format!("{repo}"),
                    };
                    let detail = match kind {
                        IndexDriftKind::SafeToFix => {
                            "index stale (run `rwv doctor --fix` to reset)"
                        }
                        IndexDriftKind::LiveStaged => {
                            "index has live staged changes (commit or stash first; \
                             not auto-fixed)"
                        }
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!("{location}: {detail}"),
                    )
                }
                CheckViolation::MissingReplayExclusion { project } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{project}: project repo missing `rwv.lock merge=rwv-ours` in .gitattributes \
                         (run `rwv doctor --fix` to add)"
                    ),
                ),
                CheckViolation::LegacyRolePrimary {
                    project,
                    manifest_path,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{project}: manifest at {} uses deprecated `role: primary`; \
                         run `rwv doctor --fix` to migrate to `role: owned`",
                        manifest_path.display()
                    ),
                ),
                CheckViolation::LegacyWorkweaveMarker { marker_path, .. } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{} is a legacy workweave marker missing `parent:`; \
                         run `rwv doctor --fix` to migrate",
                        marker_path.display()
                    ),
                ),
                CheckViolation::UnparseableProject {
                    project,
                    manifest_path,
                    message,
                } => (
                    crate::integration::Severity::Error,
                    format!(
                        "{project}: manifest at {} cannot be parsed: {message}; \
                         fix the YAML by hand and re-run `rwv doctor`",
                        manifest_path.display()
                    ),
                ),
                CheckViolation::WorkingTreeDrift {
                    workweave,
                    repo,
                    kind,
                } => {
                    let location = match workweave {
                        Some(ww) => format!("{ww}/{repo}"),
                        None => format!("{repo}"),
                    };
                    let detail = match kind {
                        WorkingTreeDriftKind::SafeToFix => {
                            "working tree stale (run `rwv doctor --fix` to restore)"
                        }
                        WorkingTreeDriftKind::LiveEdits => {
                            "working tree has live edits (commit or stash first; \
                             not auto-fixed)"
                        }
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!("{location}: {detail}"),
                    )
                }
                CheckViolation::DanglingActiveProject {
                    project,
                    missing_dir,
                } => (
                    crate::integration::Severity::Error,
                    format!(
                        "active project `{}` is set in `.rwv-active` but `{}` does not exist; \
                         run `rwv activate <existing-project>` or remove `.rwv-active`",
                        project,
                        missing_dir.display()
                    ),
                ),
                CheckViolation::WorkweaveTreeIntegrity {
                    workweave_dir,
                    sub_kind,
                } => {
                    let msg = match &sub_kind {
                        WorkweaveTreeIntegrityKind::DanglingParent { parent_path } => format!(
                            "{}: marker `parent` points to `{}` which does not exist; \
                             run `rwv doctor --fix` to re-point parent to primary",
                            workweave_dir.display(),
                            parent_path.display()
                        ),
                        WorkweaveTreeIntegrityKind::ParentChainAnomaly { detail } => format!(
                            "{}: workweave parent-chain anomaly: {}",
                            workweave_dir.display(),
                            detail
                        ),
                        WorkweaveTreeIntegrityKind::UnregisteredDir => format!(
                            "{}: directory under workweaves parent has no `.rwv-workweave` marker",
                            workweave_dir.display()
                        ),
                        WorkweaveTreeIntegrityKind::ForeignPrimary { marker_primary } => format!(
                            "{}: marker `primary` (`{}`) does not match this workspace; \
                             this workweave may have been copied from another machine",
                            workweave_dir.display(),
                            marker_primary.display()
                        ),
                    };
                    (crate::integration::Severity::Warning, msg)
                }
                CheckViolation::Provenance {
                    project,
                    repo,
                    sub_kind,
                } => match sub_kind {
                    ProvenanceKind::OriginUrlMismatch {
                        manifest_url,
                        actual_url,
                        is_reference_role,
                    } => {
                        let suffix = if is_reference_role {
                            " (note: reference-role repos may intentionally use a different \
                             remote — verify before re-pointing)"
                        } else {
                            ""
                        };
                        (
                            crate::integration::Severity::Warning,
                            format!(
                                "{project}: {repo}: origin URL mismatch — manifest has \
                                 `{manifest_url}`, clone has `{actual_url}`; pushes may target \
                                 the wrong remote (report-only; update the manifest or \
                                 re-point the remote to converge){suffix}",
                            ),
                        )
                    }
                    ProvenanceKind::LockShaUnreachable { sha } => (
                        crate::integration::Severity::Error,
                        format!(
                            "{project}: {repo}: lock pins SHA {sha} which is absent from \
                             the local object store; the canonical store is missing the pinned \
                             revision — refresh it from its remote (fetch, not sync)",
                        ),
                    ),
                },
                CheckViolation::CloneTopology {
                    workspace_path,
                    repo,
                    sub_kind,
                } => {
                    // All clone-topology violations are tier-0 (object-store
                    // identity wrong). Repair is an object-store migration —
                    // out of scope of --fix per the alpha guideline. Severity
                    // is Error because every higher-tier check silently
                    // operates on incoherent input under these violations.
                    let msg = match &sub_kind {
                        CloneTopologyKind::StandaloneInWorkweave { store_path } => format!(
                            "clone-topology: standalone clone of `{repo}` lives in a workweave \
                             ({}); the canonical store should be at `<weave>/{repo}` not under \
                             `.workweaves/` (report-only; repair requires object-store re-parenting \
                             — re-clone into the canonical slot and reconnect workweaves)",
                            store_path.display(),
                        ),
                        CloneTopologyKind::DisconnectedWeaveClone {
                            weave_store_path,
                            other_store_path,
                        } => format!(
                            "clone-topology: weave-path clone of `{repo}` at {} is disconnected \
                             — workweave checkouts of this repo use a different canonical store \
                             ({}); the weave clone publishes an unread object DAG \
                             (report-only; repair requires object-store re-parenting)",
                            weave_store_path.display(),
                            other_store_path.display(),
                        ),
                        CloneTopologyKind::WrongParentWorktree {
                            expected_store_path,
                            actual_store_path,
                        } => format!(
                            "clone-topology: workweave checkout of `{repo}` at {} is linked into \
                             {} instead of the weave canonical {}; cross-DAG merged-checks \
                             silently answer `no` \
                             (report-only; repair requires object-store re-parenting)",
                            workspace_path.display(),
                            actual_store_path.display(),
                            expected_store_path.display(),
                        ),
                        CloneTopologyKind::WeaveCloneIsWorktree { actual_store_path } => format!(
                            "clone-topology: weave-path slot for `{repo}` at {} is itself a \
                             linked worktree of {}; the canonical store has migrated out of \
                             the manifest slot (report-only; repair requires object-store \
                             re-parenting — re-clone into the canonical slot)",
                            workspace_path.display(),
                            actual_store_path.display(),
                        ),
                    };
                    (crate::integration::Severity::Error, msg)
                }
                CheckViolation::BranchDiscipline {
                    repo_path,
                    sub_kind,
                } => {
                    let msg = match &sub_kind {
                        BranchDisciplineKind::SharedBranch {
                            actual_branch,
                            expected_prefix,
                        } => format!(
                            "{}: workweave checkout is on shared-branch `{}` (expected an \
                             ephemeral branch under `{}/`); manual `git switch` inside a \
                             workweave breaks the I3 branch-ownership invariant — \
                             use `git switch -c {}/main` to move onto an ephemeral branch \
                             (report-only; no rwv --fix path)",
                            repo_path.display(),
                            actual_branch,
                            expected_prefix,
                            expected_prefix
                        ),
                        BranchDisciplineKind::ForeignEphemeral {
                            actual_branch,
                            expected_prefix,
                        } => format!(
                            "{}: workweave checkout is on `{}`, which names a different \
                             workweave (expected an ephemeral branch under `{}/`); \
                             use `git switch -c {}/main` to move onto the correct \
                             ephemeral branch (report-only; no rwv --fix path)",
                            repo_path.display(),
                            actual_branch,
                            expected_prefix,
                            expected_prefix
                        ),
                        BranchDisciplineKind::Detached => format!(
                            "{}: workweave checkout is in detached-HEAD state (expected an \
                             ephemeral branch); use `git switch -c <project>--<workweave>/main` \
                             to attach to an ephemeral branch \
                             (report-only; no rwv --fix path)",
                            repo_path.display()
                        ),
                        BranchDisciplineKind::EphemeralAtPrimary { actual_branch } => format!(
                            "{}: canonical clone is checked out on ephemeral branch `{}`; \
                             canonicals must sit on a non-ephemeral branch — \
                             use `git switch <tracking-branch>` to restore \
                             (report-only; no rwv --fix path)",
                            repo_path.display(),
                            actual_branch
                        ),
                        BranchDisciplineKind::StaleEphemeralBranchSafe {
                            branch,
                            workweave_name,
                        } => format!(
                            "{}: stale ephemeral branch `{}` for deleted workweave `{}` \
                             (safe class — tip is reachable from the primary branch; \
                             `rwv doctor --fix` will delete it)",
                            repo_path.display(),
                            branch,
                            workweave_name
                        ),
                        BranchDisciplineKind::StaleEphemeralBranchLive {
                            branch,
                            workweave_name,
                            tip_sha,
                        } => {
                            // Live-class: tip carries commits not reachable
                            // from the primary; `doctor --fix` must not touch
                            // it. Mark the issue accordingly so the integration
                            // runner's user-held-issues partition leaves it
                            // alone.
                            safe_to_fix = false;
                            format!(
                                "{}: stale ephemeral branch `{}` for deleted workweave `{}` \
                                 carries unique commits at tip `{}` (live class — `--fix` \
                                 will not touch this; recover or delete by hand)",
                                repo_path.display(),
                                branch,
                                workweave_name,
                                tip_sha
                            )
                        }
                    };
                    (crate::integration::Severity::Warning, msg)
                }
                CheckViolation::StaleWorktreeRegistration {
                    workweave,
                    repo,
                    missing_path,
                } => {
                    let location = match workweave {
                        Some(ww) => format!("{ww}/{repo}"),
                        None => format!("{repo}"),
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!(
                            "{location}: stale-worktree-registration: missing path \
                             {} (run `rwv doctor --fix` to prune)",
                            missing_path.display()
                        ),
                    )
                }
                CheckViolation::StaleOpState {
                    workspace_dir,
                    started_at,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{}/.rwv-op: stale-op-state present (started_at={started_at}); \
                         resume with `rwv sync --continue` or roll back with `rwv abort`. \
                         Never auto-fixed — another terminal may be mid-conflict-resolution.",
                        workspace_dir.display()
                    ),
                ),
                CheckViolation::DeadOpLease {
                    workspace_dir,
                    op_id,
                    recorded_owner,
                    sub_kind,
                    created_at,
                } => {
                    let cause = match &sub_kind {
                        DeadOpLeaseKind::OwnerRecordAbsent => format!(
                            "recorded owner workspace {} has no `.rwv-op` file",
                            recorded_owner.display()
                        ),
                        DeadOpLeaseKind::OwnerOpIdMismatch { owner_op_id } => format!(
                            "recorded owner workspace {} holds a different op \
                             (owner op_id={owner_op_id}, lease op_id={op_id})",
                            recorded_owner.display()
                        ),
                    };
                    // Surface lease age as observability-only context, matching
                    // the StaleOpState pattern (RFC3339 raw + humanized elapsed).
                    // Never used as a decision input — classification is structural.
                    let age_str = match created_at.as_deref() {
                        Some(ts) => format!(
                            " (created_at={ts}, age={})",
                            crate::op_state::elapsed_since(ts)
                        ),
                        None => String::new(),
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!(
                            "{}/.rwv-op-lease: dead-op-lease op_id={op_id}{age_str} — {cause}; \
                             safe to auto-fix with `rwv doctor --fix` (removes the lease file).",
                            workspace_dir.display()
                        ),
                    )
                }
                CheckViolation::OrphanedSavepoint {
                    workweave,
                    repo,
                    op_id,
                    sub_kind,
                } => {
                    let location = match workweave {
                        Some(ww) => format!("{ww}/{repo}"),
                        None => format!("{repo}"),
                    };
                    let detail = match sub_kind {
                        OrphanedSavepointKind::Redundant => {
                            "orphaned-savepoint (redundant — tip reachable from HEAD; \
                             run `rwv doctor --fix` to drop)"
                        }
                        OrphanedSavepointKind::Live => {
                            "orphaned-savepoint (last pointer to unreachable commits; \
                             keep — recover or delete by hand with `git branch <name> <sha>`)"
                        }
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!("{location}: {detail} op_id={op_id}"),
                    )
                }
                CheckViolation::CargoVersionSkew {
                    crate_name,
                    occurrences,
                } => {
                    // Report-not-mandate (Finding 3): skew is informational.
                    // Warning severity so doctor's exit stays 0 by default;
                    // safe_to_fix is true only in the trivial sense that
                    // there's nothing for --fix to do (rwv cannot mandate
                    // versions in sovereign repos) — leaving it true keeps
                    // the finding out of the not-safe-to-fix "user held"
                    // bucket meant for pen-holding conflicts.
                    let mut msg = format!(
                        "cargo version skew: `{crate_name}` required at differing versions across \
                         members (report-only — rwv cannot mandate versions in sovereign repos)"
                    );
                    for occ in &occurrences {
                        msg.push_str(&format!("\n  - {}: {}", occ.member, occ.requirement));
                    }
                    (crate::integration::Severity::Warning, msg)
                }
                CheckViolation::CargoPatchShadowing {
                    weave_config,
                    member_config,
                    registry,
                    crate_name,
                } => {
                    // Report-only precheck: cargo's closest-config-wins per-key
                    // shadowing means the member config silently defeats the
                    // weave-level entry. Cargo does not warn; its version-
                    // mismatch diagnostic actively misleads (blames crates.io
                    // — probe P6). This finding is what agents/scripts key on
                    // before generating derived patches.
                    (
                        crate::integration::Severity::Warning,
                        format!(
                            "cargo patch shadowing: `[patch.{registry}].{crate_name}` in {} \
                             silently defeats the weave-level entry in {} \
                             (cargo merges .cargo/config.toml closest-wins per key, with no \
                             warning when a patch is inert)",
                            member_config.display(),
                            weave_config.display(),
                        ),
                    )
                }
                CheckViolation::MissingCanonicalClone {
                    workweave,
                    repo,
                    canonical_path,
                } => {
                    // The canonical clone for this repo is gone; the worktree
                    // cannot be classified. Do NOT advise commit/stash — the
                    // git layer is broken and commands will fail anyway. Point
                    // at the same manual re-clone repair as DanglingReference.
                    safe_to_fix = false;
                    (
                        crate::integration::Severity::Warning,
                        format!(
                            "{workweave}/{repo}: canonical clone for `{repo}` is absent \
                             (expected at {}) — this worktree cannot be classified; \
                             run `rwv fetch` from the workspace root to \
                             re-materialize it, then re-run `rwv doctor` to verify",
                            canonical_path.display()
                        ),
                    )
                }
                CheckViolation::UninitializedSubmodule {
                    workweave,
                    repo,
                    empty_paths,
                } => {
                    let fix_cmd = format!(
                        "git -C <worktree>/{repo} submodule update --init --recursive"
                    );
                    (
                        crate::integration::Severity::Warning,
                        format!(
                            "{workweave}/{repo}: submodules not initialized — \
                             {n} submodule path(s) are empty on disk: {paths}; \
                             fix: `{fix_cmd}`",
                            n = empty_paths.len(),
                            paths = empty_paths.join(", "),
                        ),
                    )
                }
            };
            Issue {
                integration: "core".into(),
                severity,
                message,
                safe_to_fix,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Index-drift helpers
// ---------------------------------------------------------------------------

/// Combined index + working-tree drift classification using one git
/// invocation for the common "clean worktree" fast path.
///
/// The doctor's per-worktree scan calls both [`classify_index_drift`] and
/// [`classify_working_tree_drift`] for every worktree it sees. Each of those
/// functions previously paid the cost of a fresh `git diff-index` subprocess
/// just to determine "clean vs dirty" before doing any further work — so the
/// minimum cost per worktree was two `git` invocations, and at workspace
/// scale (80+ workweaves × ~13 repos = ~1000 worktrees) the doubled
/// process-startup overhead dominated wall-clock time.
///
/// This helper runs `git status --porcelain` once. If the output is empty
/// (workspace and index both clean against HEAD), both classifiers return
/// `None` without spawning any further subprocesses. If the output is
/// non-empty, the caller falls back to the per-kind classifiers, which
/// continue to issue the detail probes needed to bucket the drift into
/// `SafeToFix` / `LiveStaged` / `LiveEdits`.
///
/// Returns `(index_drift, working_tree_drift)`. Either side may be `None`
/// independently — e.g., index dirty but working tree clean.
pub fn classify_drift(repo: &Path) -> (Option<IndexDriftKind>, Option<WorkingTreeDriftKind>) {
    // `git status --porcelain` (without `-z`) emits one record per dirty
    // entry; an empty stdout means both the index and the working tree
    // match HEAD, which is the overwhelmingly common case in a healthy
    // workspace. We only need the empty/non-empty signal here; the
    // detail-level classification still goes through the per-kind helpers
    // below when drift is detected.
    let status_out = git_command()
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .stderr(std::process::Stdio::null())
        .output();
    let porcelain = match status_out {
        Ok(out) if out.status.success() => out.stdout,
        // Treat any error reading status as "potentially dirty" and fall
        // through to the per-kind classifiers — they're already defensive
        // about transient git failures.
        _ => {
            return (
                classify_index_drift(repo),
                classify_working_tree_drift(repo),
            )
        }
    };
    if porcelain.is_empty() {
        return (None, None);
    }
    (
        classify_index_drift(repo),
        classify_working_tree_drift(repo),
    )
}

/// Classify the index-drift state of a git repo at `repo`.
///
/// Returns `None` when the index matches HEAD (no drift).  Otherwise returns
/// `Some(IndexDriftKind)` — either `SafeToFix` (index tree is an ancestor
/// commit's tree, safely replaceable) or `LiveStaged` (user has staged content
/// that is not a committed tree; must not be auto-fixed).
///
/// For workspace-scale scans (where most worktrees are clean), prefer
/// [`classify_drift`] — it short-circuits both this check and
/// [`classify_working_tree_drift`] with a single `git status` invocation.
pub fn classify_index_drift(repo: &Path) -> Option<IndexDriftKind> {
    // Exit-0 means index matches HEAD tree — no drift.
    let clean = git_command()
        .args(["diff-index", "--cached", "--exit-code", "HEAD"])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true); // assume clean if git unavailable
    if clean {
        return None;
    }

    // Index differs from HEAD. Determine the current index tree SHA.
    let index_tree = match git_command().arg("write-tree").current_dir(repo).output() {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout)
            .unwrap_or_default()
            .trim()
            .to_owned(),
        _ => return Some(IndexDriftKind::LiveStaged), // conservative
    };

    // Safety check: is the index tree the tree of some recent ancestor commit?
    // Bounded to 200 ancestors to keep performance acceptable on deep histories.
    let ancestor_trees = match git_command()
        .args(["log", "--format=%T", "-200", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout).unwrap_or_default(),
        _ => return Some(IndexDriftKind::LiveStaged),
    };

    if ancestor_trees.lines().any(|t| t.trim() == index_tree) {
        Some(IndexDriftKind::SafeToFix)
    } else {
        Some(IndexDriftKind::LiveStaged)
    }
}

/// Reset the index to match HEAD, leaving the working tree and HEAD untouched.
///
/// Only call after confirming `classify_index_drift` returns `SafeToFix`.
/// Uses bare `git reset` (equivalent to `git reset --mixed HEAD`).
pub fn reset_index_to_head(repo: &Path) -> anyhow::Result<()> {
    let out = git_command()
        .arg("reset")
        .current_dir(repo)
        .output()
        .context("failed to run git reset")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git reset failed in {}: {}", repo.display(), stderr.trim());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Canonical-clone presence detection
// ---------------------------------------------------------------------------

/// Detect whether a git worktree at `repo` has its canonical clone missing.
///
/// In a `git worktree add` checkout, the `.git` entry inside the directory is
/// a *text file* (not a directory) whose content is a single line of the form:
/// `gitdir: /abs/path/to/canonical/.git/worktrees/<name>`
///
/// The canonical clone directory is the parent of the `.git` directory
/// referenced in that `gitdir:` line. When a primary clone is removed out-of-
/// band the `.git` worktrees sub-entry and the entire canonical `.git` tree go
/// with it — git commands in the linked worktree then fail.
///
/// Returns `Some(canonical_dir)` when the worktree is a linked worktree (`.git`
/// is a file, not a directory) **and** the canonical directory is absent.
/// Returns `None` when:
/// - `.git` is a directory (this is the canonical clone itself — not a worktree)
/// - the `gitdir:` target exists on disk (canonical is present; normal case)
/// - the `gitdir:` line cannot be parsed (defensive: caller should not skip
///   drift classification for unknowns)
pub fn worktree_canonical_clone_missing(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");

    // If .git is a directory this is the canonical clone, not a linked worktree.
    // Nothing to detect here.
    if dot_git.is_dir() {
        return None;
    }

    // Read the .git file. If it does not exist or is not readable, fall through
    // to the normal drift classifiers (defensive: don't suppress findings for
    // repos we cannot inspect).
    let content = std::fs::read_to_string(&dot_git).ok()?;

    // Extract the `gitdir:` value (absolute path to the worktrees admin dir
    // inside the canonical .git).
    let gitdir_path = content
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))?
        .trim();
    if gitdir_path.is_empty() {
        return None;
    }

    // The gitdir path looks like: <canonical>/.git/worktrees/<name>
    // Walk up two levels to reach the canonical .git dir, then one more to
    // reach the canonical clone directory.
    //
    //   gitdir_path  =  /ws/primary/github/repo/.git/worktrees/ww--name
    //   git_dir      =  /ws/primary/github/repo/.git/worktrees/ww--name/../..
    //                =  /ws/primary/github/repo/.git
    //   canonical    =  /ws/primary/github/repo
    //
    // We go up exactly three levels: `<name>` → `worktrees` → `.git` →
    // canonical clone dir, so the reported path names the repo directory
    // (consistent with DanglingReference's repair target).
    let gitdir = std::path::Path::new(gitdir_path);
    let canonical = gitdir.parent()?.parent()?.parent()?;

    if !canonical.exists() {
        Some(canonical.to_path_buf())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Working-tree-drift helpers
// ---------------------------------------------------------------------------

/// Classify the working-tree-drift state of a git repo at `repo`.
///
/// Returns `None` when the working tree matches HEAD (no drift). Otherwise
/// returns `Some(WorkingTreeDriftKind)` — either `SafeToFix` (all modified
/// files' on-disk content matches a reachable committed blob) or `LiveEdits`
/// (at least one file has content not found in recent ancestors; must not be
/// auto-fixed).
///
/// Uses `git diff-index HEAD` (without `--cached`) so detection works
/// regardless of whether index drift has already been resolved.
///
/// For workspace-scale scans, prefer [`classify_drift`] — it short-circuits
/// the clean-worktree case with a single `git status` invocation that
/// covers both this check and [`classify_index_drift`].
pub fn classify_working_tree_drift(repo: &Path) -> Option<WorkingTreeDriftKind> {
    // Exit-0 means working tree matches HEAD — no drift.
    let clean = git_command()
        .args(["diff-index", "--exit-code", "HEAD"])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true);
    if clean {
        return None;
    }

    // Use --name-status to distinguish two cases:
    //   D = file exists in HEAD but is absent from the working tree — content is
    //       in HEAD and by definition reachable; always safe to restore.
    //   M = file differs between HEAD and working tree — must verify the on-disk
    //       blob is reachable before treating it as safely fixable.
    let status_out = match git_command()
        .args(["diff-index", "--name-status", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => out,
        // Conservative fallback for unknown git failures: return LiveEdits to
        // prevent accidental auto-fix of content we cannot inspect. The
        // canonical-clone-missing case is pre-classified upstream (before this
        // function is called) via `worktree_canonical_clone_missing`, so this
        // arm should not fire for that root cause in practice.
        _ => return Some(WorkingTreeDriftKind::LiveEdits),
    };
    let mut modified_files: Vec<String> = Vec::new();
    let mut has_entries = false;
    for line in String::from_utf8_lossy(&status_out.stdout).lines() {
        if line.is_empty() {
            continue;
        }
        has_entries = true;
        let mut parts = line.splitn(2, '\t');
        let status = parts.next().unwrap_or("").trim();
        let path = parts.next().unwrap_or("").trim();
        match status {
            "D" => {
                // Deleted from working tree; restore from HEAD → safely fixable.
            }
            "M" | "T" => {
                modified_files.push(path.to_owned());
            }
            _ => return Some(WorkingTreeDriftKind::LiveEdits),
        }
    }
    if !has_entries {
        return None;
    }
    if modified_files.is_empty() {
        // Only D (deleted-from-WT) entries — always safely restorable.
        return Some(WorkingTreeDriftKind::SafeToFix);
    }

    // Gather all reachable object SHAs from the last 200 commits.
    let objects_out = match git_command()
        .args(["rev-list", "--objects", "-n", "200", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return Some(WorkingTreeDriftKind::LiveEdits),
    };
    let reachable: std::collections::HashSet<String> = String::from_utf8(objects_out.stdout)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(|s| s.to_owned()))
        .collect();

    // For each M file, verify its on-disk blob is reachable.
    for file in &modified_files {
        let hash_out = match git_command()
            .args(["hash-object", file])
            .current_dir(repo)
            .output()
        {
            Ok(out) if out.status.success() => out,
            _ => return Some(WorkingTreeDriftKind::LiveEdits),
        };
        let blob_sha = String::from_utf8_lossy(&hash_out.stdout).trim().to_owned();
        if !reachable.contains(&blob_sha) {
            return Some(WorkingTreeDriftKind::LiveEdits);
        }
    }

    Some(WorkingTreeDriftKind::SafeToFix)
}

/// Restore working-tree files to match HEAD.
///
/// Only call after confirming `classify_working_tree_drift` returns `SafeToFix`.
/// Restores each tracked file that differs from HEAD using
/// `git checkout HEAD -- <files>`, leaving unstaged files and the index alone.
pub fn restore_working_tree_to_head(repo: &Path) -> anyhow::Result<()> {
    let modified_out = git_command()
        .args(["diff-index", "--name-only", "HEAD"])
        .current_dir(repo)
        .output()
        .context("failed to run git diff-index")?;
    let files: Vec<String> = String::from_utf8_lossy(&modified_out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_owned())
        .collect();
    if files.is_empty() {
        return Ok(());
    }

    let mut args = vec!["checkout".to_owned(), "HEAD".to_owned(), "--".to_owned()];
    args.extend(files);
    let out = git_command()
        .args(&args)
        .current_dir(repo)
        .output()
        .context("failed to run git checkout")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "git checkout HEAD -- <files> failed in {}: {}",
            repo.display(),
            stderr.trim()
        );
    }
    Ok(())
}

/// Execute `rwv doctor --locked` for the current workspace context.
///
/// Compares each repo's HEAD SHA against its `rwv.lock` entry. Outputs per-repo
/// status to stdout. Returns `Ok(true)` if any repo's tip differs from its lock
/// entry (exit 1), `Ok(false)` if all match (exit 0).
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_check_locked(ctx: &crate::workspace::WorkspaceContext) -> anyhow::Result<bool> {
    use crate::git::GitVcs;
    use crate::manifest::Project;
    use crate::vcs::Vcs;
    use crate::workspace::Checkout;

    let git = GitVcs;
    let workspace_dir = ctx.active_path().to_path_buf();

    let project_names: Vec<String> = match &ctx.checkout {
        Checkout::Primary { project: Some(p) } => vec![p.as_str().to_owned()],
        Checkout::Workweave { project, .. } => vec![project.as_str().to_owned()],
        Checkout::Primary { project: None } => {
            crate::workspace::discover_project_paths(&workspace_dir)
        }
    };

    let mut any_drift = false;

    for pname in &project_names {
        let project_dir = workspace_dir.join("projects").join(pname);
        let project = match Project::from_dir(&project_dir) {
            Ok(p) => p,
            Err(e) => {
                // Warn and skip; `rwv doctor` surfaces the canonical
                // `unparseable-project` violation for this project.
                eprintln!(
                    "warning: skipping project {pname}: manifest unreadable ({e}); \
                     run `rwv doctor` for details"
                );
                continue;
            }
        };

        let raw_lock = match project.lock {
            Some(l) => l,
            None => continue,
        };

        // Resolve lock entries against on-disk repos. Repos whose revision
        // can't be resolved (unknown tag/branch) come back in `failures`
        // along with the raw string, so we can report them with a distinct
        // "unknown revision" message. The raw lock is iterated below so
        // that "missing on disk" entries (which are silently dropped by
        // `resolve_versions`) still get a diagnostic.
        let raw_entries = raw_lock.repo_map().clone();
        let (resolved, failures) = raw_lock.resolve_versions(&workspace_dir);
        let unresolved: std::collections::BTreeMap<RepoPath, crate::vcs::RawRevisionId> =
            failures.into_iter().collect();

        for (repo_path, raw_entry) in &raw_entries {
            let repo_abs = workspace_dir.join(repo_path.as_path());

            let actual = match git.head_revision(&repo_abs) {
                Ok(rev) => rev,
                Err(_) => {
                    println!(
                        "{repo_path}: missing on disk (lock pins {}); run `rwv sync` to materialize",
                        raw_entry.version
                    );
                    any_drift = true;
                    continue;
                }
            };

            if let Some(raw_rev) = unresolved.get(repo_path) {
                println!("{repo_path}: lock pins unknown revision {}", raw_rev);
                any_drift = true;
                continue;
            }

            let Some(resolved_entry) = resolved.get_entry(repo_path) else {
                // Resolve dropped this entry without surfacing it as a
                // failure — shouldn't happen for an on-disk repo with a
                // valid rev, but stay defensive.
                continue;
            };

            if actual == resolved_entry.version {
                println!("{repo_path}: ok");
            } else {
                println!(
                    "{repo_path}: tip {} ≠ lock {}",
                    actual, resolved_entry.version
                );
                any_drift = true;
            }
        }
    }

    Ok(any_drift)
}

/// Execute `rwv doctor` for the current workspace context.
///
/// Scans registry directories for repos on disk, loads all project manifests,
/// runs convention checks and integration check hooks, then displays issues.
/// When `fix` is `true`, safely-auto-fixable index-drift cases are remediated
/// in place with `git reset` (index ← HEAD, working tree untouched).
///
/// When `scope_all` is `false` (the default), only the active project is
/// checked: stale locks, dangling references, and integration hooks are
/// scoped to that project, and orphan detection is skipped (a repo absent
/// from the active project may belong to another project). Pass `scope_all =
/// true` (via `--all`) to reproduce the previous weave-wide behaviour,
/// including orphan detection across every project.
///
/// Returns `Ok(true)` if there are errors (exit 1), `Ok(false)` if clean.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_check(
    ctx: &crate::workspace::WorkspaceContext,
    fix: bool,
    scope_all: bool,
) -> anyhow::Result<bool> {
    use crate::git::GitVcs;
    use crate::integration::Severity;
    use crate::integration_runner::run_checks;
    use crate::manifest::Project;
    use crate::vcs::Vcs;
    use crate::workspace::{Checkout, WorkspaceSession};

    let workspace_dir = ctx.active_path().to_path_buf();

    // Dangling active-project check: if `.rwv-active` names a project whose
    // `projects/<name>/` directory does not exist on disk, report it as an
    // error. With `--fix`, clear `.rwv-active` so the workspace is no longer
    // broken. Either way, doctor continues to report other violations.
    let dangling_active: Option<CheckViolation> = {
        use crate::workspace::read_active_project;
        if let Some(active_name) = read_active_project(ctx.primary_path()) {
            let project_dir = ctx
                .primary_path()
                .join("projects")
                .join(active_name.as_str());
            if !project_dir.is_dir() {
                Some(CheckViolation::DanglingActiveProject {
                    project: active_name.clone(),
                    missing_dir: project_dir.clone(),
                })
            } else {
                None
            }
        } else {
            None
        }
    };

    // Build session (runs builtin_registries → scan_repos_on_disk → discover_project_paths).
    let session = WorkspaceSession::new(&workspace_dir);
    let git = GitVcs;

    // Legacy `role: primary` scan + optional --fix migration. Runs before
    // `Project::from_dir`, since manifests with the legacy spelling fail
    // to parse now that the back-compat alias is gone. With `--fix`, the
    // rewrite happens here so subsequent loaders see the migrated
    // manifests.
    // In default (project-scoped) mode with an active project, only report
    // findings for that project. Without an active project, report all
    // (matches the fall-through in project loading).
    let active_project_name: Option<crate::manifest::ProjectName> = ctx.active_project().cloned();
    let legacy_role_primary_all = scan_workspace_for_legacy_role_primary(&workspace_dir);
    let legacy_role_primary: Vec<_> = if scope_all || active_project_name.is_none() {
        legacy_role_primary_all
    } else {
        legacy_role_primary_all
            .into_iter()
            .filter(|f| {
                active_project_name
                    .as_ref()
                    .map(|a| f.project.as_str() == a.as_str())
                    .unwrap_or(true)
            })
            .collect()
    };
    let mut legacy_role_primary_warnings: Vec<(crate::manifest::ProjectName, PathBuf)> = Vec::new();
    let mut legacy_role_primary_errors: Vec<(crate::manifest::ProjectName, String)> = Vec::new();
    for finding in &legacy_role_primary {
        if fix {
            match fix_legacy_role_primary(&finding.manifest_path) {
                Ok(0) => {
                    // Race: detector saw the legacy spelling but the
                    // rewriter found none. Treat as a no-op.
                }
                Ok(count) => {
                    println!(
                        "[fixed] core: migrated {count} `role: primary` → `role: owned` in {}",
                        finding.manifest_path.display()
                    );
                }
                Err(e) => {
                    legacy_role_primary_errors.push((finding.project.clone(), e.to_string()));
                }
            }
        } else {
            legacy_role_primary_warnings
                .push((finding.project.clone(), finding.manifest_path.clone()));
        }
    }

    // Legacy workweave-marker scan + optional --fix migration. Runs from the
    // primary weave only (workweave markers live in the workweave-parent dir
    // which is sibling to the primary). Scans even from a workweave CWD so
    // `rwv doctor --fix` works from wherever the operator runs it.
    let legacy_ww_markers = scan_for_legacy_workweave_markers(ctx.primary_path());
    let mut legacy_ww_marker_warnings: Vec<LegacyWorkweaveMarkerFile> = Vec::new();
    let mut legacy_ww_marker_errors: Vec<String> = Vec::new();
    for finding in &legacy_ww_markers {
        if fix {
            match fix_legacy_workweave_marker(finding) {
                Ok(true) => {
                    println!(
                        "[fixed] core: appended `parent:` to {}",
                        finding.marker_path.display()
                    );
                }
                Ok(false) => {
                    // Race: already had parent: by the time we tried to fix.
                }
                Err(e) => {
                    legacy_ww_marker_errors.push(e.to_string());
                }
            }
        } else {
            legacy_ww_marker_warnings.push(finding.clone());
        }
    }

    // Resolve HEAD revisions for each repo on disk. Errors are kept (not
    // dropped) so that `find_violations` can flag on-disk repos whose HEAD
    // could not be read (corrupted, mid-rebase, permissions). Audit B4.
    let mut head_revisions = BTreeMap::new();
    let mut head_read_failures: Vec<(RepoPath, String)> = Vec::new();
    for repo_path in session.repos_on_disk() {
        let abs = workspace_dir.join(repo_path.as_path());
        match git.head_revision(&abs) {
            Ok(rev) => {
                head_revisions.insert(repo_path.clone(), rev);
            }
            Err(e) => {
                head_read_failures.push((repo_path.clone(), e.to_string()));
            }
        }
    }

    // Determine which project(s) to load. In default (project-scoped) mode
    // only the active project is loaded so that stale-lock, dangling-reference,
    // and integration findings stay within the project the operator cares about.
    // Under `--all`, every project is loaded and weave-wide checks (orphan
    // detection, cross-project stale locks) run as before.
    let active_project_name: Option<crate::manifest::ProjectName> = ctx.active_project().cloned();

    // Load project manifest(s) from projects/*/rwv.yaml.
    // In default mode: only the active project (identified by active_project_name).
    // In --all mode: every project under projects/.
    let projects_dir = workspace_dir.join("projects");
    let mut projects = Vec::new();
    let mut known_repos = BTreeSet::new();
    let mut lock_resolve_failures: Vec<(crate::manifest::ProjectName, RepoPath)> = Vec::new();
    // Projects whose rwv.yaml exists but fails to parse — surfaced as
    // `unparseable-project` violations so the workspace is never silent-clean
    // when a manifest is broken.
    let mut unparseable_projects: Vec<(crate::manifest::ProjectName, PathBuf, String)> = Vec::new();

    let mut resolved_locks: std::collections::HashMap<
        crate::manifest::ProjectName,
        crate::manifest::ResolvedLockFile,
    > = std::collections::HashMap::new();

    if projects_dir.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&projects_dir)?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let manifest_path = project_dir.join("rwv.yaml");
            if !manifest_path.exists() {
                continue;
            }
            let rel_dir = project_dir
                .strip_prefix(&workspace_dir)
                .unwrap_or(&project_dir);
            let name_from_rel = rel_dir
                .strip_prefix("projects")
                .unwrap_or(rel_dir)
                .to_string_lossy()
                .into_owned();

            // In default mode with an active project, skip other projects so
            // that stale-lock, dangling-reference, and integration findings
            // stay within the project the operator cares about.
            // If no active project is set, fall back to loading every project
            // (preserves behaviour in simple workspaces that don't call `rwv
            // activate`).
            if !scope_all {
                if let Some(ref active) = active_project_name {
                    if name_from_rel != active.as_str() {
                        continue;
                    }
                }
                // No active project → don't skip; fall through to load all.
            }

            match Project::from_dir(&project_dir) {
                Ok(project) => {
                    // Resolve lock entries against on-disk repos so the
                    // canonical-SHA equality used by `find_violations` works
                    // uniformly for tag-form, branch-form, and SHA-form locks.
                    //
                    // B3: capture unresolvable entries instead of discarding
                    // them. An unresolvable rev means the local clone has
                    // never seen the SHA/tag the lock pinned; without this
                    // diagnostic, `find_violations` either flags nothing
                    // (no head_revisions entry) or falsely reports StaleLock
                    // by comparing the raw tag string against a real SHA.
                    if let Some(raw_lock) = project.lock.clone() {
                        let project_name_for_issue = project.name.clone();
                        let (resolved, failures) = raw_lock.resolve_versions(&workspace_dir);
                        for (unresolved, _raw_rev) in failures {
                            lock_resolve_failures
                                .push((project_name_for_issue.clone(), unresolved));
                        }
                        resolved_locks.insert(project.name.clone(), resolved);
                    }

                    for repo_path in project.manifest.iter_repo_paths() {
                        known_repos.insert(repo_path.clone());
                    }
                    projects.push(project);
                }
                Err(e) => {
                    // Surface unparseable manifests as a violation so
                    // operators get a clear signal instead of zero findings
                    // (which looks identical to a healthy project). --fix
                    // does not auto-repair broken YAML; the operator must
                    // fix by hand and re-run.
                    // Defer: collect violations after the input is built.
                    // We push directly into `all_issues` at display time.
                    // Store for now in a side-channel parallel to the other
                    // failure vecs already used in this function.
                    unparseable_projects.push((
                        crate::manifest::ProjectName::new(name_from_rel),
                        manifest_path,
                        e.to_string(),
                    ));
                }
            }
        }
    }

    // Orphan detection requires all projects to be loaded (otherwise repos
    // that belong to non-loaded projects look orphaned). Run it when:
    //   - `--all` was passed (all projects loaded), OR
    //   - no active project is set (fall-through path also loaded all projects).
    let loaded_all_projects = scope_all || active_project_name.is_none();

    // Build CheckInput and find violations
    let input = CheckInput {
        known_repos,
        repos_on_disk: session.repos_on_disk().to_vec(),
        projects,
        head_revisions,
        resolved_locks,
        check_orphans: loaded_all_projects,
    };

    let mut violations = find_violations(&input);
    for (project, manifest_path) in &legacy_role_primary_warnings {
        violations.push(CheckViolation::LegacyRolePrimary {
            project: project.clone(),
            manifest_path: manifest_path.clone(),
        });
    }

    // Dangling active-project: emit the violation or apply the --fix.
    // Fix errors are collected here so they can be appended to all_issues
    // after the violations batch is converted below.
    let mut dangling_fix_errors: Vec<String> = Vec::new();
    // Branch-discipline --fix errors collected the same way.
    let mut all_issues_branch_discipline_errors: Vec<String> = Vec::new();
    if let Some(CheckViolation::DanglingActiveProject {
        project: dap_project,
        missing_dir: dap_dir,
    }) = dangling_active
    {
        if fix {
            let active_file = ctx.primary_path().join(".rwv-active");
            match std::fs::remove_file(&active_file) {
                Ok(()) => println!(
                    "[fixed] core: cleared `.rwv-active` (was pointing at missing project `{}`)",
                    dap_project
                ),
                Err(e) => {
                    dangling_fix_errors.push(format!(
                        "dangling-active-project fix failed for `{}`: {e}",
                        dap_project
                    ));
                }
            }
        } else {
            violations.push(CheckViolation::DanglingActiveProject {
                project: dap_project,
                missing_dir: dap_dir,
            });
        }
    }

    for finding in &legacy_ww_marker_warnings {
        violations.push(CheckViolation::LegacyWorkweaveMarker {
            marker_path: finding.marker_path.clone(),
            primary: finding.primary.clone(),
        });
    }

    // Workweave-tree integrity: dangling parent, chain anomalies, unregistered
    // dirs, foreign-primary markers. Chain-anomaly / unregistered-dir /
    // foreign-primary are report-only; `dangling-parent` gains a `--fix` that
    // re-points the child's marker to primary. Run from the primary weave so
    // the scan covers all workweaves belonging to this workspace.
    let mut dangling_parent_fix_errors: Vec<String> = Vec::new();
    for v in scan_workweave_tree_integrity(ctx.primary_path()) {
        if fix {
            if let CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir,
                sub_kind: WorkweaveTreeIntegrityKind::DanglingParent { .. },
            } = &v
            {
                match fix_dangling_parent(workweave_dir, ctx.primary_path()) {
                    Ok(true) => {
                        println!(
                            "[fixed] core: re-pointed dangling parent of {} to primary",
                            workweave_dir.display()
                        );
                        continue;
                    }
                    Ok(false) => {
                        // Race: parent existed by the time we tried to fix.
                        continue;
                    }
                    Err(e) => {
                        dangling_parent_fix_errors.push(e.to_string());
                        // Fall through and still report the violation so the
                        // operator sees the unresolved dangling parent.
                    }
                }
            }
        }
        violations.push(v);
    }

    // Provenance checks: origin-url-mismatch and lock-sha-unreachable.
    // Always report-only (no --fix path).
    for v in scan_provenance(&workspace_dir, &input.projects) {
        violations.push(v);
    }

    // Uninitialized-submodule findings (R23 GAP). Only meaningful from the
    // primary weave (workweave checkouts are reached via list_workweave_dirs).
    // Report-only: fix is a single git command named in the message.
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        for v in scan_uninitialized_submodules_in_workweaves(ctx.primary_path(), &input.projects) {
            violations.push(v);
        }
    }

    // Clone-topology: tier-0 invariants from
    // `docs/explanation/joints/clone-topology.md`. Compares each manifest
    // repo's canonical store at `<weave>/<repo>` against every workweave
    // checkout's store. Report-only (repair is an object-store migration).
    for v in scan_clone_topology(ctx.primary_path(), &input.known_repos) {
        violations.push(v);
    }

    // Branch-discipline: (a) workweave-branch, (b) ephemeral-at-primary,
    // (c) stale-ephemeral-branches. (a) and (b) are report-only; (c) splits
    // into safe-class (deletable under --fix) and live-class (never
    // auto-deleted). The --fix path is applied below before violations are
    // emitted so a successful delete is reported as `[fixed]` instead of
    // surfacing the corresponding warning.
    //
    // Scope: when scope_all is false and an active project is set, filter
    // findings to only those belonging to the active project. This mirrors
    // the legacy_role_primary filter above and prevents the doctor scoped
    // to a single active project from touching another project's stale
    // ephemeral branches.
    let mut branch_discipline_violations = scan_branch_discipline(ctx.primary_path(), &git);
    if !scope_all {
        if let Some(ref active) = active_project_name {
            branch_discipline_violations.retain(|v| {
                branch_discipline_in_scope(
                    v,
                    ctx.primary_path(),
                    active.as_str(),
                    &input.known_repos,
                )
            });
        }
    }
    if fix {
        // Pass the active-project scope into the deleter so it only removes
        // branches that belong to the active project.
        let fix_active = if scope_all {
            None
        } else {
            active_project_name.as_ref().map(|n| n.as_str())
        };
        let (deleted, fix_errs) =
            fix_stale_ephemeral_branches(ctx.primary_path(), &git, fix_active, &input.known_repos);
        for (repo_path, branch) in &deleted {
            println!(
                "[fixed] core: deleted safe-class stale ephemeral branch `{}` in {}",
                branch,
                repo_path.display()
            );
        }
        let deleted_keys: std::collections::HashSet<(PathBuf, String)> =
            deleted.into_iter().collect();
        // Drop safe-class findings the fix path successfully deleted so
        // the operator doesn't see both `[fixed]` and a paired warning.
        branch_discipline_violations.retain(|v| match v {
            CheckViolation::BranchDiscipline {
                repo_path,
                sub_kind: BranchDisciplineKind::StaleEphemeralBranchSafe { branch, .. },
            } => !deleted_keys.contains(&(repo_path.clone(), branch.clone())),
            _ => true,
        });
        for msg in fix_errs {
            all_issues_branch_discipline_errors.push(msg);
        }
    }
    violations.extend(branch_discipline_violations);

    // Surface unparseable manifests as first-class violations so the
    // workspace is never reported as "clean" when a manifest is broken.
    for (project, manifest_path, message) in &unparseable_projects {
        violations.push(CheckViolation::UnparseableProject {
            project: project.clone(),
            manifest_path: manifest_path.clone(),
            message: message.clone(),
        });
    }

    let mut all_issues = violations_to_issues(violations);
    for msg in all_issues_branch_discipline_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: msg,
            safe_to_fix: true,
        });
    }

    for msg in dangling_fix_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: msg,
            safe_to_fix: true,
        });
    }

    for msg in dangling_parent_fix_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!("dangling-parent --fix failed: {msg}"),
            safe_to_fix: true,
        });
    }

    for (project_name, err) in &legacy_role_primary_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!("{project_name}: legacy `role: primary` fix failed: {err}"),
            safe_to_fix: true,
        });
    }
    for err in &legacy_ww_marker_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!("legacy workweave marker fix failed: {err}"),
            safe_to_fix: true,
        });
    }

    // B3: surface lock entries that couldn't be resolved against the local
    // repo. Doctor is the diagnostic of last resort — swallowing this signal
    // is exactly the wrong place to drop information.
    for (project_name, repo_path) in &lock_resolve_failures {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!(
                "{project_name}: lock references unknown revision for {repo_path}; run `rwv lock` or fetch"
            ),
            safe_to_fix: true,
        });
    }

    // B4: surface on-disk repos whose HEAD could not be read. Previously the
    // Err was silently dropped, so `find_violations` produced zero
    // violations for these repos and doctor reported clean.
    for (repo_path, err_msg) in &head_read_failures {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!("{repo_path}: HEAD unreadable ({err_msg})"),
            safe_to_fix: true,
        });
    }

    // Run integration check hooks for each project
    let builtin = crate::integrations::builtin_integrations();
    let integrations: Vec<&dyn crate::integration::Integration> =
        builtin.iter().map(|b| b.as_ref()).collect();

    for project in &input.projects {
        let detection_cache = crate::integration_runner::build_detection_cache(
            &workspace_dir,
            project.manifest.iter_entries(),
        );
        let ctx_base = session.context_base(
            &workspace_dir,
            &project.name,
            &detection_cache,
            project.manifest.workweave.as_ref(),
        );
        let integration_issues = run_checks(&integrations, &project.manifest, &ctx_base);
        all_issues.extend(integration_issues);

        // Cargo version-skew observatory + patch-shadowing precheck.
        // Warning-only findings; feed the same `violations_to_issues` path
        // the built-in `CheckViolation`s use so exit-status and formatting
        // stay consistent. Emitted here (not in `find_violations`) because
        // the scan needs an `IntegrationContext` — it walks the cargo
        // integration's members-with-config expansion.
        {
            let default_cfg = crate::manifest::IntegrationConfig::default();
            let cargo_cfg = project
                .manifest
                .integrations
                .get("cargo-workspace")
                .unwrap_or(&default_cfg);
            let cargo_ctx = ctx_base.build_context(cargo_cfg, &project.manifest);
            match scan_cargo_ecosystem(&cargo_ctx) {
                Ok(vs) => all_issues.extend(violations_to_issues(vs)),
                Err(e) => all_issues.push(Issue {
                    integration: "cargo-workspace".into(),
                    severity: Severity::Warning,
                    message: format!("skew/patch scan failed: {e}"),
                    safe_to_fix: true,
                }),
            }
        }

        // Trigger-model drift check (see `trigger-model.md`): the integrations'
        // `verify()` pass reports drift between on-disk managed/generated content
        // and what `activate()` would produce. Under `--fix`, doctor invokes the
        // intent-mode write path to regenerate safe-to-fix drift. Without `--fix`,
        // all drift findings surface as warnings — `doctor` is the detector and
        // the fixer.
        //
        // USER-HELD findings (`safe_to_fix = false`) are always surfaced as-is,
        // even under `--fix` — these are cases where the user holds the pen on a
        // managed file region and auto-repair would silently destroy user content.
        // Doctor never auto-takes-over a user-held file.
        let verify_issues = crate::integration_runner::run_verifications(
            &integrations,
            &project.manifest,
            &ctx_base,
        );
        // Split into auto-fixable (safe_to_fix=true) and user-held (safe_to_fix=false).
        let (fixable_issues, user_held_issues): (Vec<_>, Vec<_>) =
            verify_issues.into_iter().partition(|i| i.safe_to_fix);
        // USER-HELD findings always surface — we never auto-rewrite them.
        all_issues.extend(user_held_issues);
        if fix && !fixable_issues.is_empty() {
            // Regenerate by running intent-mode activation bound to THIS weave
            // (primary or workweave). This is the canonical write path; any
            // integration whose verify() flagged safe-to-fix drift will re-author
            // its content here.
            //
            // Weave-binding mirrors the surfacing-fix precedent below: the
            // repair primitive must be pointed at the same weave dir the
            // detector scanned, never at ctx.primary_path() unconditionally.
            // From a workweave-checkout context, the naive `activate_intent`
            // path targets primary and would silently rewrite the PRIMARY
            // project's managed files from inside a workweave — breaking the
            // isolation contract that makes workweave-scoped repair risk-free.
            // `activate_workweave_intent` is the workweave-bound sibling: it
            // runs the same `run_activations` pass with
            // `output_dir = workweave/projects/<project>` and skips install
            // hooks. From primary we use the primary path.
            let result = if matches!(ctx.checkout, Checkout::Workweave { .. }) {
                crate::activate::activate_workweave_intent(project.name.as_str(), &workspace_dir)
            } else {
                crate::activate::activate_intent(project.name.as_str(), ctx)
            };
            match result {
                Ok(()) => println!(
                    "[fixed] core: regenerated integration content for project `{}` (drift detected)",
                    project.name
                ),
                Err(e) => all_issues.push(Issue {
                    integration: "core".into(),
                    severity: Severity::Error,
                    message: format!(
                        "doctor --fix: failed to regenerate integration content for `{}`: {e}",
                        project.name
                    ),
                    safe_to_fix: true,
                }),
            }
        } else {
            all_issues.extend(fixable_issues);
        }

        // Framework-level Axis-1 surfacing check. Distinct from the
        // per-integration `verify()` pass above, which only sees Axis-2 content
        // drift: nothing there asserts that the *symlinks* the surfacing layer
        // should have created actually exist and resolve. This pass is a SECOND
        // CONSUMER of the same `generated_files() ∪ managed_files()` union that
        // drives symlink CREATION — it lives in the framework (byte-identical
        // across all integrations) rather than being duplicated into each
        // `verify()`. It scopes to `workspace_dir` (= `ctx.active_path()`), so
        // run at primary it checks primary's surfacing and run in a workweave it
        // checks that workweave's. The recovery hatch is `--fix`, which re-runs
        // the surfacing PRIMITIVE (`surface_symlinks`) bound to this weave
        // directory — NOT `activate_intent`, since project re-selection is a
        // primary-only step-1 concept forbidden inside a workweave.
        let in_workweave = matches!(ctx.checkout, Checkout::Workweave { .. });
        let surfacing_issues = crate::activate::verify_surfacing(
            &workspace_dir,
            &project.name,
            &project.manifest,
            in_workweave,
        );
        let (surf_fixable, surf_user_held): (Vec<_>, Vec<_>) =
            surfacing_issues.into_iter().partition(|i| i.safe_to_fix);
        // A real file/dir occupying a surfacing path is user-held — never
        // auto-clobbered; always surfaced as-is.
        all_issues.extend(surf_user_held);
        if fix && !surf_fixable.is_empty() {
            // Re-surface by re-running the step-2 surfacing primitive against
            // this weave directory. Unlike `activate_intent`, this writes no
            // `.rwv-active` and authors no content — it only (re)creates the
            // owner-scoped symlinks, which is valid in any weave (it is exactly
            // what workweave-create runs at creation).
            match crate::activate::surface_symlinks(
                &workspace_dir,
                &project.name,
                &project.manifest,
                in_workweave,
            ) {
                Ok(()) => println!(
                    "[fixed] core: re-surfaced symlinks for project `{}` (missing/mis-resolved surfacing)",
                    project.name
                ),
                Err(e) => all_issues.push(Issue {
                    integration: "core".into(),
                    severity: Severity::Error,
                    message: format!(
                        "doctor --fix: failed to re-surface symlinks for `{}`: {e}",
                        project.name
                    ),
                    safe_to_fix: true,
                }),
            }
        } else {
            all_issues.extend(surf_fixable);
        }
    }

    // Index-drift + working-tree-drift detection.
    //
    // Scan list: every materialized worktree referenced by a manifest. From
    // the primary weave we additionally enumerate each workweave's repos.
    // The build loop dedupes by absolute path so multiple projects that share
    // a repo only pay one round of git subprocess cost per physical worktree.
    // The two drift kinds are then classified in a single
    // pass using [`classify_drift`], which collapses the common
    // "worktree is clean" case to one `git status` invocation instead of two
    // back-to-back `git diff-index` invocations.
    let mut index_scan: Vec<(Option<String>, std::path::PathBuf, String)> = Vec::new();
    let mut scan_seen: std::collections::HashSet<(Option<String>, std::path::PathBuf)> =
        std::collections::HashSet::new();

    for project in &input.projects {
        for repo_path in project.manifest.iter_repo_paths() {
            let abs = workspace_dir.join(repo_path.as_path());
            if abs.exists() && scan_seen.insert((None, abs.clone())) {
                index_scan.push((None, abs, repo_path.to_string()));
            }
        }
    }

    // From the primary weave: also scan every known workweave.
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        for (ww_name, ww_dir) in crate::workweave::list_workweave_dirs(ctx.primary_path()) {
            for project in &input.projects {
                for repo_path in project.manifest.iter_repo_paths() {
                    let abs = ww_dir.join(repo_path.as_path());
                    if abs.exists() && scan_seen.insert((Some(ww_name.clone()), abs.clone())) {
                        index_scan.push((Some(ww_name.clone()), abs, repo_path.to_string()));
                    }
                }
            }
        }
    }
    drop(scan_seen);

    // Progress output: workspace-scale doctor runs (80+ workweaves × ~13
    // repos) were previously silent for many seconds. Emit a heartbeat
    // to stderr so the operator can tell "in progress" from "hung". The
    // line goes to stderr to keep stdout free of noise for the human-
    // facing report below. JSON callers go through `run_check_json` and
    // don't see this.
    let total_scans = index_scan.len();
    let progress_every = total_scans.div_ceil(20).max(1);
    if total_scans > 0 {
        eprintln!("doctor: scanning {total_scans} worktree(s) for drift...");
    }

    for (i, (ww_label, repo_abs, repo_display)) in index_scan.iter().enumerate() {
        if total_scans >= 50 && (i + 1) % progress_every == 0 {
            eprintln!("doctor: scanned {}/{total_scans}", i + 1);
        }
        let location = match ww_label {
            Some(ww) => format!("{ww}/{repo_display}"),
            None => repo_display.clone(),
        };

        // Pre-flight: detect a missing canonical clone before attempting any
        // git classification. When the primary clone directory was removed out-
        // of-band, all git commands in this linked worktree will fail — the
        // previous behaviour was to misattribute the failure as `LiveEdits`.
        // Only linked worktrees (workweave repos) can have a missing canonical;
        // skip the check for primary-weave entries (ww_label == None) since
        // those ARE the canonical clones.
        if ww_label.is_some() {
            if let Some(canonical_path) = worktree_canonical_clone_missing(repo_abs) {
                all_issues.push(Issue {
                    integration: "core".into(),
                    severity: Severity::Warning,
                    message: format!(
                        "{location}: canonical clone for `{repo_display}` is absent \
                         (expected at {}) — this worktree cannot be classified; \
                         run `rwv fetch` from the workspace root to \
                         re-materialize it, then re-run `rwv doctor` to verify",
                        canonical_path.display()
                    ),
                    safe_to_fix: false,
                });
                continue; // skip drift classification for this worktree
            }
        }

        let (idx_drift, wt_drift) = classify_drift(repo_abs);

        if let Some(drift_kind) = idx_drift {
            match drift_kind {
                IndexDriftKind::SafeToFix => {
                    if fix {
                        match reset_index_to_head(repo_abs) {
                            Ok(()) => println!("[fixed] core: index refreshed for {location}"),
                            Err(e) => all_issues.push(Issue {
                                integration: "core".into(),
                                severity: Severity::Error,
                                message: format!("{location}: index fix failed: {e}"),
                                safe_to_fix: true,
                            }),
                        }
                    } else {
                        all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Warning,
                            message: format!("{location}: index stale (safe to --fix)"),
                            safe_to_fix: true,
                        });
                    }
                }
                IndexDriftKind::LiveStaged => {
                    all_issues.push(Issue {
                        integration: "core".into(),
                        severity: Severity::Warning,
                        message: format!(
                            "{location}: index has live staged changes (manual review)"
                        ),
                        safe_to_fix: true,
                    });
                }
            }
        }

        if let Some(drift_kind) = wt_drift {
            match drift_kind {
                WorkingTreeDriftKind::SafeToFix => {
                    if fix {
                        match restore_working_tree_to_head(repo_abs) {
                            Ok(()) => {
                                println!("[fixed] core: working tree refreshed for {location}")
                            }
                            Err(e) => all_issues.push(Issue {
                                integration: "core".into(),
                                severity: Severity::Error,
                                message: format!("{location}: working-tree fix failed: {e}"),
                                safe_to_fix: true,
                            }),
                        }
                    } else {
                        all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Warning,
                            message: format!("{location}: working tree stale (safe to --fix)"),
                            safe_to_fix: true,
                        });
                    }
                }
                WorkingTreeDriftKind::LiveEdits => {
                    all_issues.push(Issue {
                        integration: "core".into(),
                        severity: Severity::Warning,
                        message: format!("{location}: working tree has live edits (manual review)"),
                        safe_to_fix: true,
                    });
                }
            }
        }
    }

    // State hygiene: stale worktree registrations, stale `.rwv-op`,
    // orphaned savepoints. See `scan_state_hygiene` for the rationale.
    // Mirrors the index_scan enumeration (every manifest repo, every
    // workweave) and additionally pulls in the project repo
    // (`projects/<name>/`), which also carries savepoints (sync.rs:1573).
    let mut hygiene_targets: Vec<StateHygieneScanTarget> = Vec::new();
    let mut hygiene_seen: std::collections::HashSet<(Option<String>, std::path::PathBuf)> =
        std::collections::HashSet::new();
    for project in &input.projects {
        for repo_path in project.manifest.iter_repo_paths() {
            let abs = workspace_dir.join(repo_path.as_path());
            if abs.exists() && hygiene_seen.insert((None, abs.clone())) {
                hygiene_targets.push(StateHygieneScanTarget {
                    workweave: None,
                    abs,
                    repo: repo_path.clone(),
                });
            }
        }
    }
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        for (ww_name, ww_dir) in crate::workweave::list_workweave_dirs(ctx.primary_path()) {
            for project in &input.projects {
                for repo_path in project.manifest.iter_repo_paths() {
                    let abs = ww_dir.join(repo_path.as_path());
                    if abs.exists() && hygiene_seen.insert((Some(ww_name.clone()), abs.clone())) {
                        hygiene_targets.push(StateHygieneScanTarget {
                            workweave: Some(WorkweaveName::new(ww_name.clone())),
                            abs,
                            repo: repo_path.clone(),
                        });
                    }
                }
            }
        }
    }
    // Project repos: `projects/<name>/` is itself a git repo carrying
    // savepoints (sync.rs creates one there for the CWD project at sync
    // time). The manifest enumeration above doesn't include it because
    // the project repo is not a `repositories:` entry.
    for project in &input.projects {
        let pname = project.name.as_str();
        let project_rel = format!("projects/{pname}");
        let project_repo_path = match RepoPath::new(project_rel.clone()) {
            Ok(rp) => rp,
            Err(_) => continue,
        };
        let project_abs = workspace_dir.join(&project_rel);
        if project_abs.is_dir() && hygiene_seen.insert((None, project_abs.clone())) {
            hygiene_targets.push(StateHygieneScanTarget {
                workweave: None,
                abs: project_abs,
                repo: project_repo_path.clone(),
            });
        }
        if matches!(ctx.checkout, Checkout::Primary { .. }) {
            for (ww_name, ww_dir) in crate::workweave::list_workweave_dirs(ctx.primary_path()) {
                let ww_project_abs = ww_dir.join(&project_rel);
                if ww_project_abs.is_dir()
                    && hygiene_seen.insert((Some(ww_name.clone()), ww_project_abs.clone()))
                {
                    hygiene_targets.push(StateHygieneScanTarget {
                        workweave: Some(WorkweaveName::new(ww_name.clone())),
                        abs: ww_project_abs,
                        repo: project_repo_path.clone(),
                    });
                }
            }
        }
    }
    drop(hygiene_seen);

    // Op-state scan: check the CWD workspace and every workweave for `.rwv-op`.
    let mut hygiene_op_state_targets: Vec<StateHygieneOpStateTarget> = Vec::new();
    hygiene_op_state_targets.push(StateHygieneOpStateTarget {
        workspace_dir: workspace_dir.clone(),
    });
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        for (_ww_name, ww_dir) in crate::workweave::list_workweave_dirs(ctx.primary_path()) {
            // Dedupe against the active workspace dir (operator may run
            // from inside a workweave, in which case it's already added).
            if !hygiene_op_state_targets
                .iter()
                .any(|t| t.workspace_dir == ww_dir)
            {
                hygiene_op_state_targets.push(StateHygieneOpStateTarget {
                    workspace_dir: ww_dir,
                });
            }
        }
    }

    let hygiene_violations = scan_state_hygiene(&git, &hygiene_targets, &hygiene_op_state_targets);
    // Mirror the project-scoped target dedupe used elsewhere: build a quick
    // map from `(workweave, repo)` to absolute path so the `--fix` path can
    // call into the right repo. The keys come straight from the targets we
    // just scanned.
    let target_lookup: std::collections::HashMap<(Option<String>, String), std::path::PathBuf> =
        hygiene_targets
            .iter()
            .map(|t| {
                (
                    (
                        t.workweave.as_ref().map(|w| w.to_string()),
                        t.repo.to_string(),
                    ),
                    t.abs.clone(),
                )
            })
            .collect();

    for violation in hygiene_violations {
        // Try the --fix path first when enabled; the auto-fixable set is:
        //   - `StaleWorktreeRegistration`
        //   - `OrphanedSavepoint { Redundant }`
        //   - `DeadOpLease` (routes to op_state::fix_dead_lease directly, no
        //     repo lookup needed)
        // See `fix_state_hygiene` for the policy rationale.
        let fix_attempted = if fix {
            match &violation {
                CheckViolation::StaleWorktreeRegistration {
                    workweave, repo, ..
                }
                | CheckViolation::OrphanedSavepoint {
                    workweave,
                    repo,
                    sub_kind: OrphanedSavepointKind::Redundant,
                    ..
                } => {
                    let key = (workweave.as_ref().map(|w| w.to_string()), repo.to_string());
                    match target_lookup.get(&key) {
                        Some(repo_abs) => match fix_state_hygiene(&git, &violation, repo_abs) {
                            Ok(true) => {
                                let (kind_label, extra) = match &violation {
                                    CheckViolation::StaleWorktreeRegistration { .. } => {
                                        ("stale-worktree-registration", "pruned".to_string())
                                    }
                                    CheckViolation::OrphanedSavepoint { op_id, .. } => {
                                        ("orphaned-savepoint", format!("dropped op_id={op_id}"))
                                    }
                                    _ => ("state-hygiene", String::new()),
                                };
                                let location = match (workweave, repo) {
                                    (Some(ww), r) => format!("{ww}/{r}"),
                                    (None, r) => r.to_string(),
                                };
                                println!("[fixed] core: {kind_label} for {location}: {extra}");
                                true
                            }
                            Ok(false) => false,
                            Err(e) => {
                                all_issues.push(Issue {
                                    integration: "core".into(),
                                    severity: Severity::Error,
                                    message: format!("state-hygiene --fix failed: {e}"),
                                    safe_to_fix: true,
                                });
                                true
                            }
                        },
                        None => false,
                    }
                }
                CheckViolation::DeadOpLease {
                    workspace_dir,
                    op_id,
                    ..
                } => {
                    // fix_state_hygiene ignores repo_abs for this variant —
                    // it operates on the lease's workspace_dir directly. We
                    // pass workspace_dir as a stand-in so the signature is
                    // unchanged.
                    match fix_state_hygiene(&git, &violation, workspace_dir) {
                        Ok(true) => {
                            println!(
                                "[fixed] core: dead-op-lease for {}: removed lease (op_id={op_id})",
                                workspace_dir.display()
                            );
                            true
                        }
                        Ok(false) => false,
                        Err(e) => {
                            all_issues.push(Issue {
                                integration: "core".into(),
                                severity: Severity::Error,
                                message: format!("state-hygiene --fix failed: {e}"),
                                safe_to_fix: true,
                            });
                            true
                        }
                    }
                }
                _ => false,
            }
        } else {
            false
        };
        if !fix_attempted {
            all_issues.extend(violations_to_issues(vec![violation]));
        }
    }

    // Replay-exclusion check: each project repo should carry
    // `rwv.lock merge=rwv-ours` in `.gitattributes` AND the paired
    // `merge.rwv-ours.driver=true` durable config. Older projects
    // don't have either; the legacy spelling `rwv.lock merge=ours`
    // (from before the rename to a namespaced driver) may still be
    // present, which `--fix` migrates in place. `--fix` writes the
    // line in place
    // (idempotent — re-running on a fixed repo is a no-op) and, when
    // the change is a legacy-name migration, commits it (skipping the
    // commit when the repo has unrelated staged work so we never
    // bundle a user's WIP).
    for project in &input.projects {
        let project_repo = workspace_dir.join("projects").join(project.name.as_str());
        if !project_repo.is_dir() {
            continue;
        }

        // Detect legacy `rwv.lock merge=ours` in the working tree so
        // `--fix` can migrate + commit even when the on-disk `.gitattributes`
        // carries the old spelling.
        let has_legacy = crate::git::has_working_tree_legacy_replay_exclusion(
            &project_repo,
            std::path::Path::new("rwv.lock"),
        )
        .unwrap_or(false);

        match git.has_replay_exclusion(&project_repo, std::path::Path::new("rwv.lock")) {
            Ok(true) if !has_legacy => {}
            Ok(has_new) => {
                if fix {
                    // `set_replay_exclusion` migrates a legacy line to
                    // the new name in place (rewrite, not append-alongside)
                    // and appends the new needle when neither is present.
                    // Idempotent when the new line is already the only one.
                    match git.set_replay_exclusion(&project_repo, std::path::Path::new("rwv.lock"))
                    {
                        Ok(()) => {
                            if has_legacy {
                                // Migration path: also commit the change so
                                // the invariant (which reads the *committed*
                                // form) sees the new spelling on the next
                                // sync. Skip the commit if the repo carries
                                // unrelated staged changes — user work must
                                // not be bundled with an rwv-authored fix.
                                match commit_replay_exclusion_migration(&project_repo) {
                                    Ok(CommitOutcome::Committed) => println!(
                                        "[fixed] core: migrated `rwv.lock merge=ours` → \
                                         `rwv.lock merge=rwv-ours` in {}/.gitattributes (committed)",
                                        project.name
                                    ),
                                    Ok(CommitOutcome::SkippedUnrelatedStaged) => println!(
                                        "[fixed] core: migrated `rwv.lock merge=ours` → \
                                         `rwv.lock merge=rwv-ours` in {}/.gitattributes (NOT committed: \
                                         project repo has unrelated staged changes; commit them, then \
                                         re-run `rwv doctor --fix` to complete the migration)",
                                        project.name
                                    ),
                                    Ok(CommitOutcome::NothingToCommit) => println!(
                                        "[fixed] core: migrated `rwv.lock merge=ours` → \
                                         `rwv.lock merge=rwv-ours` in {}/.gitattributes",
                                        project.name
                                    ),
                                    Err(e) => all_issues.push(Issue {
                                        integration: "core".into(),
                                        severity: Severity::Error,
                                        message: format!(
                                            "{}: migrated .gitattributes but commit failed: {e}",
                                            project.name
                                        ),
                                        safe_to_fix: true,
                                    }),
                                }
                            } else if !has_new {
                                println!(
                                    "[fixed] core: wrote `rwv.lock merge=rwv-ours` to {}/.gitattributes",
                                    project.name
                                );
                            }
                        }
                        Err(e) => all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Error,
                            message: format!(
                                "{}: failed to write replay-exclusion: {e}",
                                project.name
                            ),
                            safe_to_fix: true,
                        }),
                    }
                } else {
                    let msg = if has_legacy {
                        format!(
                            "{}: project repo has legacy `rwv.lock merge=ours` in .gitattributes; \
                             the driver was renamed to close a global-config collision hazard \
                             (run `rwv doctor --fix` to migrate to `rwv.lock merge=rwv-ours` \
                             and commit)",
                            project.name
                        )
                    } else {
                        format!(
                            "{}: project repo missing `rwv.lock merge=rwv-ours` in .gitattributes \
                             (run `rwv doctor --fix` to add)",
                            project.name
                        )
                    };
                    all_issues.push(Issue {
                        integration: "core".into(),
                        severity: Severity::Warning,
                        message: msg,
                        safe_to_fix: true,
                    });
                }
            }
            Err(e) => all_issues.push(Issue {
                integration: "core".into(),
                severity: Severity::Warning,
                message: format!(
                    "{}: failed to read .gitattributes for replay-exclusion check: {e}",
                    project.name
                ),
                safe_to_fix: true,
            }),
        }

        // Durable-config plant: `merge.rwv-ours.driver` config keeps the
        // exclusion working across bare `git rebase --continue` (see
        // `plant_rwv_merge_driver_config`). Detect via `git config --get`;
        // `--fix` writes both `.driver` and `.name` entries.
        match crate::git::has_rwv_merge_driver_config(&project_repo) {
            Ok(true) => {}
            Ok(false) => {
                if fix {
                    match crate::git::plant_rwv_merge_driver_config(&project_repo) {
                        Ok(()) => println!(
                            "[fixed] core: planted `{}` config in {}",
                            crate::git::RWV_MERGE_DRIVER_CONFIG_KEY,
                            project.name
                        ),
                        Err(e) => all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Error,
                            message: format!(
                                "{}: failed to plant `{}`: {e}",
                                project.name,
                                crate::git::RWV_MERGE_DRIVER_CONFIG_KEY
                            ),
                            safe_to_fix: true,
                        }),
                    }
                } else {
                    all_issues.push(Issue {
                        integration: "core".into(),
                        severity: Severity::Warning,
                        message: format!(
                            "{}: project repo missing `{}` config \
                             (defines the `rwv-ours` merge driver used by bare \
                             `git rebase --continue`; run `rwv doctor --fix` to plant)",
                            project.name,
                            crate::git::RWV_MERGE_DRIVER_CONFIG_KEY,
                        ),
                        safe_to_fix: true,
                    });
                }
            }
            Err(e) => all_issues.push(Issue {
                integration: "core".into(),
                severity: Severity::Warning,
                message: format!(
                    "{}: failed to read `{}` config: {e}",
                    project.name,
                    crate::git::RWV_MERGE_DRIVER_CONFIG_KEY,
                ),
                safe_to_fix: true,
            }),
        }
    }

    // Display issues and determine exit status
    let mut has_errors = false;
    for issue in &all_issues {
        let prefix = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => {
                has_errors = true;
                "error"
            }
        };
        // The tests check stdout for the issue messages
        println!("[{prefix}] {}: {}", issue.integration, issue.message);
    }

    Ok(has_errors)
}

/// Build the JSON payload for `rwv doctor --json` from a vector of
/// violations and the resolved workspace context. Extracted from
/// [`run_check_json`] so tests can drive the serialization shape without
/// reaching for a real workspace on disk.
///
/// Returns `{ "$schema": ..., "violations": [...] }`.
pub fn build_doctor_json(
    violations: Vec<CheckViolation>,
    workspace_dir: &Path,
    workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
) -> serde_json::Value {
    let outputs: Vec<ViolationOutput> = violations
        .into_iter()
        .map(|v| ViolationOutput::from_violation(v, workspace_dir, workweave_dirs))
        .collect();
    serde_json::json!({
        "$schema": DOCTOR_SCHEMA_URL,
        "violations": outputs,
    })
}

/// Collect every `CheckViolation` `rwv doctor` knows how to produce.
///
/// Mirrors the scaffolding in [`run_check`] but returns a typed enum vector
/// instead of mixing `Issue`s and `CheckViolation`s. Integration-runner
/// findings and lock-resolution / HEAD-read failures are out of scope: they
/// are not `CheckViolation` variants today and the spec explicitly excludes
/// them from `--json` (the acceptance criterion is "each `CheckViolation`
/// variant serializes").
///
/// When `scope_all` is `false`, only the active project is loaded and the
/// orphan check is skipped (matching the default scoping of `run_check`).
/// Pass `scope_all = true` (`--all`) for the weave-wide scan.
///
/// Returns `(violations, workweave_dirs)` so the caller can resolve
/// workweave-scoped `absolute_path` fields.
fn collect_doctor_violations(
    ctx: &crate::workspace::WorkspaceContext,
    scope_all: bool,
) -> anyhow::Result<(
    Vec<CheckViolation>,
    std::path::PathBuf,
    std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
)> {
    use crate::git::GitVcs;
    use crate::vcs::Vcs;
    use crate::workspace::{Checkout, WorkspaceSession};

    let workspace_dir = ctx.active_path().to_path_buf();

    let session = WorkspaceSession::new(&workspace_dir);
    let git = GitVcs;

    // Resolve HEAD revisions for each repo on disk. HEAD-read failures are
    // surfaced by the non-JSON `run_check` as `Issue`s; they have no
    // `CheckViolation` variant and are therefore not emitted under `--json`.
    let mut head_revisions = BTreeMap::new();
    for repo_path in session.repos_on_disk() {
        let abs = workspace_dir.join(repo_path.as_path());
        if let Ok(rev) = git.head_revision(&abs) {
            head_revisions.insert(repo_path.clone(), rev);
        }
    }

    // Active project name for project-scoped filtering.
    let active_project_name: Option<crate::manifest::ProjectName> = ctx.active_project().cloned();

    // Load projects + resolve lock files.
    // In default mode: only the active project. In --all mode: every project.
    let projects_dir = workspace_dir.join("projects");
    let mut projects = Vec::new();
    let mut known_repos = BTreeSet::new();
    let mut resolved_locks: std::collections::HashMap<
        crate::manifest::ProjectName,
        crate::manifest::ResolvedLockFile,
    > = std::collections::HashMap::new();
    // Unparseable manifests collected here and emitted as violations below.
    let mut unparseable_projects_json: Vec<(crate::manifest::ProjectName, PathBuf, String)> =
        Vec::new();

    if projects_dir.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&projects_dir)?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let manifest_path = project_dir.join("rwv.yaml");
            if !manifest_path.exists() {
                continue;
            }
            let rel_dir = project_dir
                .strip_prefix(&workspace_dir)
                .unwrap_or(&project_dir);
            let name_from_rel = rel_dir
                .strip_prefix("projects")
                .unwrap_or(rel_dir)
                .to_string_lossy()
                .into_owned();

            // In default mode with an active project, skip other projects.
            // If no active project is set, fall back to loading every project.
            if !scope_all {
                if let Some(ref active) = active_project_name {
                    if name_from_rel != active.as_str() {
                        continue;
                    }
                }
                // No active project → fall through to load all.
            }

            match Project::from_dir(&project_dir) {
                Ok(project) => {
                    if let Some(raw_lock) = project.lock.clone() {
                        let (resolved, _failures) = raw_lock.resolve_versions(&workspace_dir);
                        resolved_locks.insert(project.name.clone(), resolved);
                    }
                    for repo_path in project.manifest.iter_repo_paths() {
                        known_repos.insert(repo_path.clone());
                    }
                    projects.push(project);
                }
                Err(e) => {
                    unparseable_projects_json.push((
                        crate::manifest::ProjectName::new(name_from_rel),
                        manifest_path,
                        e.to_string(),
                    ));
                }
            }
        }
    }

    let loaded_all_projects_json = scope_all || active_project_name.is_none();

    let input = CheckInput {
        known_repos,
        repos_on_disk: session.repos_on_disk().to_vec(),
        projects,
        head_revisions,
        resolved_locks,
        check_orphans: loaded_all_projects_json,
    };

    let mut violations = find_violations(&input);

    // Dangling active-project: .rwv-active names a project whose directory
    // doesn't exist. The JSON channel never auto-fixes; that's for run_check.
    {
        use crate::workspace::read_active_project;
        if let Some(active_name) = read_active_project(ctx.primary_path()) {
            let project_dir = ctx
                .primary_path()
                .join("projects")
                .join(active_name.as_str());
            if !project_dir.is_dir() {
                violations.push(CheckViolation::DanglingActiveProject {
                    project: active_name,
                    missing_dir: project_dir,
                });
            }
        }
    }

    // Unparseable manifests: surface as violations in the JSON channel too.
    for (project, manifest_path, message) in unparseable_projects_json {
        violations.push(CheckViolation::UnparseableProject {
            project,
            manifest_path,
            message,
        });
    }

    // Legacy `role: primary` findings — text-scan over `projects/*/rwv.yaml`
    // since the parser rejects the spelling and a `Project` wouldn't load.
    // The JSON channel never auto-fixes; `--fix` is reserved for the
    // human-facing `run_check`.
    // In default mode with an active project, restrict to that project only.
    // Without an active project, report all (same fall-through as project loading).
    let legacy_role_all = scan_workspace_for_legacy_role_primary(&workspace_dir);
    let legacy_role_findings: Vec<_> = if loaded_all_projects_json {
        legacy_role_all
    } else {
        // scope_all=false and active project is set
        legacy_role_all
            .into_iter()
            .filter(|f| {
                active_project_name
                    .as_ref()
                    .map(|a| f.project.as_str() == a.as_str())
                    .unwrap_or(true) // no active → include all
            })
            .collect()
    };
    for finding in legacy_role_findings {
        violations.push(CheckViolation::LegacyRolePrimary {
            project: finding.project,
            manifest_path: finding.manifest_path,
        });
    }

    // Legacy workweave-marker findings — scan the workweave-parent directory.
    // These are workspace-level infrastructure checks; always run.
    for finding in scan_for_legacy_workweave_markers(ctx.primary_path()) {
        violations.push(CheckViolation::LegacyWorkweaveMarker {
            marker_path: finding.marker_path,
            primary: finding.primary,
        });
    }

    // Workweave-tree integrity findings. Workspace-level, always run.
    for v in scan_workweave_tree_integrity(ctx.primary_path()) {
        violations.push(v);
    }

    // Provenance checks: origin-url-mismatch and lock-sha-unreachable.
    // Always report-only (no --fix path).
    for v in scan_provenance(&workspace_dir, &input.projects) {
        violations.push(v);
    }

    // Cargo version-skew + patch-shadowing scans. Per-project because they
    // consume the cargo-workspace integration config; findings are always
    // Warning severity so `--json` reports them but exit-status stays 0 by
    // default (they are informational, not gates).
    {
        let session_for_cargo = crate::workspace::WorkspaceSession::new(&workspace_dir);
        for project in &input.projects {
            let detection_cache = crate::integration_runner::build_detection_cache(
                &workspace_dir,
                project.manifest.iter_entries(),
            );
            let ctx_base = session_for_cargo.context_base(
                &workspace_dir,
                &project.name,
                &detection_cache,
                project.manifest.workweave.as_ref(),
            );
            let default_cfg = crate::manifest::IntegrationConfig::default();
            let cargo_cfg = project
                .manifest
                .integrations
                .get("cargo-workspace")
                .unwrap_or(&default_cfg);
            let cargo_ctx = ctx_base.build_context(cargo_cfg, &project.manifest);
            if let Ok(vs) = scan_cargo_ecosystem(&cargo_ctx) {
                violations.extend(vs);
            }
            // Silent skip on Err — the text channel surfaces the failure;
            // JSON stays clean rather than emit a bespoke "scan-failed"
            // pseudo-record. Failure is rare (cfg deser error).
        }
    }

    // Uninitialized-submodule findings. Workspace-level, always run.
    // Only fired for workweave checkouts: `git worktree add` does not init
    // submodules, so a workweave created from a repo-with-submodules will
    // have empty submodule dirs if the create-time init failed (e.g. network).
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        for v in scan_uninitialized_submodules_in_workweaves(ctx.primary_path(), &input.projects) {
            violations.push(v);
        }
    }

    // Clone-topology findings. Tier-0 invariants from clone-topology.md;
    // workspace-level, always run.
    for v in scan_clone_topology(ctx.primary_path(), &input.known_repos) {
        violations.push(v);
    }
    // Branch-discipline findings.
    // JSON channel never auto-fixes; `--fix` is reserved for `run_check`.
    // Scope: filter to active project unless scope_all (mirrors run_check).
    for v in scan_branch_discipline(ctx.primary_path(), &git) {
        if !scope_all {
            if let Some(ref active) = active_project_name {
                if !branch_discipline_in_scope(
                    &v,
                    ctx.primary_path(),
                    active.as_str(),
                    &input.known_repos,
                ) {
                    continue;
                }
            }
        }
        violations.push(v);
    }

    // Index-drift + working-tree-drift detection. Same scan list as
    // `run_check`: CWD workspace repos plus, from the primary weave, every
    // known workweave. Dedupe by `(workweave, abs)` so multiple projects
    // referencing the same physical worktree don't multiply git subprocess
    // cost. Drift is classified via [`classify_drift`] (single `git status`
    // fast-path for clean worktrees).
    let mut workweave_dirs: std::collections::HashMap<WorkweaveName, std::path::PathBuf> =
        std::collections::HashMap::new();
    let mut index_scan: Vec<(Option<WorkweaveName>, std::path::PathBuf, RepoPath)> = Vec::new();
    let mut scan_seen: std::collections::HashSet<(Option<WorkweaveName>, std::path::PathBuf)> =
        std::collections::HashSet::new();

    for project in &input.projects {
        for repo_path in project.manifest.iter_repo_paths() {
            let abs = workspace_dir.join(repo_path.as_path());
            if abs.exists() && scan_seen.insert((None, abs.clone())) {
                index_scan.push((None, abs, repo_path.clone()));
            }
        }
    }

    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        for (ww_name_str, ww_dir) in crate::workweave::list_workweave_dirs(ctx.primary_path()) {
            let ww_name = WorkweaveName::new(ww_name_str);
            workweave_dirs.insert(ww_name.clone(), ww_dir.clone());
            for project in &input.projects {
                for repo_path in project.manifest.iter_repo_paths() {
                    let abs = ww_dir.join(repo_path.as_path());
                    if abs.exists() && scan_seen.insert((Some(ww_name.clone()), abs.clone())) {
                        index_scan.push((Some(ww_name.clone()), abs, repo_path.clone()));
                    }
                }
            }
        }
    }
    drop(scan_seen);

    for (ww_label, repo_abs, repo_path) in &index_scan {
        // Pre-flight: detect a missing canonical clone before attempting any
        // git classification — same logic as the human-facing `run_check`.
        // Only linked worktrees (workweave repos, ww_label == Some) can have a
        // missing canonical.
        if let Some(ww_name) = ww_label {
            if let Some(canonical_path) = worktree_canonical_clone_missing(repo_abs) {
                violations.push(CheckViolation::MissingCanonicalClone {
                    workweave: ww_name.clone(),
                    repo: repo_path.clone(),
                    canonical_path,
                });
                continue; // skip drift classification for this worktree
            }
        }

        let (idx_drift, wt_drift) = classify_drift(repo_abs);
        if let Some(drift_kind) = idx_drift {
            violations.push(CheckViolation::IndexDrift {
                workweave: ww_label.clone(),
                repo: repo_path.clone(),
                kind: drift_kind,
            });
        }
        if let Some(drift_kind) = wt_drift {
            violations.push(CheckViolation::WorkingTreeDrift {
                workweave: ww_label.clone(),
                repo: repo_path.clone(),
                kind: drift_kind,
            });
        }
    }

    // State hygiene: stale worktree registrations, stale `.rwv-op`,
    // orphaned savepoints. Builds the same enumeration the human-facing
    // `run_check` uses, plus the project repos.
    let mut hygiene_targets: Vec<StateHygieneScanTarget> = index_scan
        .iter()
        .map(|(ww, abs, repo)| StateHygieneScanTarget {
            workweave: ww.clone(),
            abs: abs.clone(),
            repo: repo.clone(),
        })
        .collect();
    let mut hygiene_seen_proj: std::collections::HashSet<(
        Option<WorkweaveName>,
        std::path::PathBuf,
    )> = index_scan
        .iter()
        .map(|(ww, abs, _)| (ww.clone(), abs.clone()))
        .collect();
    for project in &input.projects {
        let pname = project.name.as_str();
        let project_rel = format!("projects/{pname}");
        let project_repo_path = match RepoPath::new(project_rel.clone()) {
            Ok(rp) => rp,
            Err(_) => continue,
        };
        let project_abs = workspace_dir.join(&project_rel);
        if project_abs.is_dir() && hygiene_seen_proj.insert((None, project_abs.clone())) {
            hygiene_targets.push(StateHygieneScanTarget {
                workweave: None,
                abs: project_abs,
                repo: project_repo_path.clone(),
            });
        }
        for (ww_name, ww_dir) in workweave_dirs.iter() {
            let ww_project_abs = ww_dir.join(&project_rel);
            if ww_project_abs.is_dir()
                && hygiene_seen_proj.insert((Some(ww_name.clone()), ww_project_abs.clone()))
            {
                hygiene_targets.push(StateHygieneScanTarget {
                    workweave: Some(ww_name.clone()),
                    abs: ww_project_abs,
                    repo: project_repo_path.clone(),
                });
            }
        }
    }
    drop(hygiene_seen_proj);

    let mut hygiene_op_state_targets: Vec<StateHygieneOpStateTarget> =
        vec![StateHygieneOpStateTarget {
            workspace_dir: workspace_dir.clone(),
        }];
    for ww_dir in workweave_dirs.values() {
        if !hygiene_op_state_targets
            .iter()
            .any(|t| &t.workspace_dir == ww_dir)
        {
            hygiene_op_state_targets.push(StateHygieneOpStateTarget {
                workspace_dir: ww_dir.clone(),
            });
        }
    }

    let hygiene_violations = scan_state_hygiene(&git, &hygiene_targets, &hygiene_op_state_targets);
    violations.extend(hygiene_violations);

    // Replay-exclusion check: each project repo should carry
    // `rwv.lock merge=rwv-ours` in `.gitattributes`. A project still on
    // the legacy `merge=ours` spelling reports missing too —
    // `has_replay_exclusion` matches only the new needle so the legacy
    // line drives the same `--fix`-migrates code path via the JSON
    // channel that the text channel already exposes.
    for project in &input.projects {
        let project_repo = workspace_dir.join("projects").join(project.name.as_str());
        if !project_repo.is_dir() {
            continue;
        }
        if let Ok(false) = git.has_replay_exclusion(&project_repo, std::path::Path::new("rwv.lock"))
        {
            violations.push(CheckViolation::MissingReplayExclusion {
                project: project.name.clone(),
            });
        }
    }

    Ok((violations, workspace_dir, workweave_dirs))
}

/// Run `rwv doctor --json`.
///
/// Emits `{ "$schema": "...", "violations": [...] }` to stdout. Exit
/// semantics match [`run_check`]: returns `Ok(true)` iff any violations
/// were found, so the caller can exit non-zero.
///
/// Only `CheckViolation` variants are surfaced — integration-runner
/// findings (which are `Issue`s, not `CheckViolation`s) and ad-hoc
/// failures (HEAD-unreadable, lock-resolve failures) are intentionally
/// out of scope for the JSON channel (see the design notes for rationale).
///
/// When `scope_all` is `false` (the default), only the active project is
/// checked and orphan detection is skipped. Pass `scope_all = true` (`--all`)
/// to reproduce the weave-wide scan.
pub fn run_check_json(
    ctx: &crate::workspace::WorkspaceContext,
    scope_all: bool,
) -> anyhow::Result<bool> {
    let (violations, workspace_dir, workweave_dirs) = collect_doctor_violations(ctx, scope_all)?;
    let has_violations = !violations.is_empty();
    let payload = build_doctor_json(violations, &workspace_dir, &workweave_dirs);
    let out =
        serde_json::to_string_pretty(&payload).context("failed to serialize doctor output")?;
    println!("{out}");
    Ok(has_violations)
}
