//! Convention checks: orphaned clones, dangling refs, stale locks, index drift, working-tree drift, etc.
//!
//! `rwv doctor` builds a workspace-wide inventory from all projects, then runs
//! a series of checks. Integration check hooks are run separately.

use crate::git::git_command;
use crate::integration::Issue;
use crate::manifest::{Project, ProjectName, RepoPath, Role, WorkweaveName};
use crate::vcs::ResolvedRevisionId;
use crate::workspace::Resolution;
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

    /// An `rwv.yaml` entry with no corresponding `rwv.lock` entry. This is a
    /// coverage gap, not a freshness one — the lock is missing the repo
    /// entirely rather than pinning it to a stale revision. Only checked
    /// when the project has a lock file at all; a project with no lock yet
    /// is a separate, unlocked state.
    IncompleteLock {
        project: ProjectName,
        repo: RepoPath,
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

    /// A `.rwv-workweave-index` written before ref-ownership receipts existed
    /// (no `receipts` field) — `branch-model.md` §7.1 arm 7, the index-side
    /// twin of [`LegacyWorkweaveMarker`](Self::LegacyWorkweaveMarker).
    ///
    /// Auto-fixable: `--fix` adds the field, which is the precondition for
    /// every other arm of the migration — `RefRegistry::record_created`
    /// refuses against a legacy index rather than erasing the only signal
    /// that the migration has not run.
    ///
    /// Reported rather than refused at read (unlike the marker), because the
    /// migration has to be able to read the index it is about to migrate, and
    /// because an unmigrated index already fails closed on its own: it holds
    /// no receipts, so under R2 nothing in it is destroyable.
    LegacyWorkweaveIndex {
        /// The project whose index needs the field.
        project: ProjectName,
        /// Absolute path to the index file.
        index_path: PathBuf,
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
    /// `<project>--<workweave>` ephemeral branch, every canonical clone sits
    /// on a non-ephemeral branch, and stale ephemeral branches left over
    /// from deleted workweaves are surfaced (and removable via `--fix` only
    /// when rwv holds an ownership receipt for the ref and it falls in the
    /// safe class — an unreceipted ref is never removable, regardless of
    /// class).
    ///
    /// The check catches manual operations the clone-topology scan cannot
    /// see — e.g. `git switch main` inside a workweave, or a `branch -D`
    /// that left behind an `<project>--<dead>` branch in the canonical.
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

    /// An ownership receipt (`branch-model.md` §4.2) whose ref is not in
    /// the store it names — the benign residue of a crash between the
    /// receipt write and the ref creation.
    ///
    /// Receipts are written **before** the refs they describe, precisely so
    /// that a crash leaves this state rather than an unreceipted ref (which
    /// R2 would leave permanently undestroyable). A dangling receipt
    /// authorizes nothing: no [`crate::vcs::DeletionWarrant`] can be built
    /// against a ref that is not there. `--fix` retracts it through
    /// [`crate::workweave_index::RefRegistry::retract`].
    ///
    /// Only raised when the store is present and readable. A receipt whose
    /// *store* is gone is R4/Q14 territory (whether receipts are ever
    /// reclaimed in bulk under a store-destroy is open), so it is left
    /// alone here.
    DanglingRefReceipt {
        /// The project whose registry holds the receipt.
        project: ProjectName,
        /// Absolute path of the canonical store the receipt is keyed to.
        store_path: PathBuf,
        /// The recorded ref name that does not exist in that store.
        ref_name: String,
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
    /// A registered workweave entry whose recorded path is not a valid
    /// workweave (missing directory, missing marker, or marker validation
    /// fails). Auto-fixable: `rwv doctor --fix` prunes the stale entry.
    ///
    /// This surfaces both "workweave was deleted out-of-band with the
    /// registry left behind" and "index committed to VCS carries paths that
    /// are wrong on this machine" — the design's advisory-index doctrine
    /// depends on doctor catching both.
    ///
    /// `project` is a plain `String` on the wire because `ProjectName` does
    /// not (yet) derive `JsonSchema`; every other sub-kind uses `String`
    /// for names on the wire for the same reason.
    StaleRegistryEntry {
        /// Project the stale entry belongs to.
        project: String,
        /// The recorded name of the workweave.
        workweave_name: String,
        /// The recorded absolute path (which no longer round-trips).
        recorded_path: PathBuf,
        /// Human-readable reason the entry failed validation.
        reason: String,
    },
    /// A marker-bearing directory in a workweave container whose
    /// `(project, name)` are NOT recorded in that project's
    /// `.rwv-workweave-index`. The workweave exists on disk but the
    /// primary-side registry does not know about it. Auto-fixable via
    /// `rwv doctor --fix` (adopts the entry into the registry) — the design
    /// requires operator-consented adoption, so read paths (`list`,
    /// `delete`) deliberately do NOT auto-adopt on the fly.
    UnregisteredWorkweave {
        /// Project this orphan workweave records in its marker.
        project: String,
        /// Workweave name parsed from the directory basename.
        workweave_name: String,
    },
    /// The `.rwv-workweave-index` file at `projects/<project>/` is tracked
    /// by the project repo's VCS. The index is machine-local state and
    /// should not be committed; a checked-in copy propagates absolute
    /// paths to every clone and every workweave checkout. Report-only —
    /// `--fix` cannot un-track without touching commit history; the
    /// operator runs `git rm --cached projects/<project>/.rwv-workweave-index`
    /// and updates `.gitignore`.
    TrackedIndex {
        /// Project whose index is committed.
        project: String,
        /// Path to the tracked index file.
        index_path: PathBuf,
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
/// * (b) canonical-store attachment (`branch-model.md` §7.2) — what the
///   canonical store's HEAD is:
///   [`CanonicalHoldsLiveWorkweaveRef`](Self::CanonicalHoldsLiveWorkweaveRef),
///   [`CanonicalHoldsLeakedRef`](Self::CanonicalHoldsLeakedRef),
///   [`CanonicalDetached`](Self::CanonicalDetached).
/// * (c) stale-ephemeral-branches — a `<project>--<name>/...` branch
///   exists in a canonical clone but workweave `<name>` no longer exists
///   on disk: [`StaleEphemeralBranchSafe`](Self::StaleEphemeralBranchSafe)
///   (auto-fixable by `--fix`),
///   [`StaleEphemeralBranchLive`](Self::StaleEphemeralBranchLive)
///   (carries unique commits; never auto-deleted), or
///   [`StaleEphemeralBranchUnowned`](Self::StaleEphemeralBranchUnowned)
///   (rwv holds no receipt for it; never auto-deleted). The safe/live split
///   applies the doctrine in `docs/explanation/joints/shared-refs-drift.md`
///   to refs: a tip that is an ancestor of the primary's tracking-branch
///   tip carries no unique work and is safely removable; a tip with
///   commits not reachable from the primary is live work and must be left
///   alone.
///
/// # Ownership is by record, never by name shape (R2)
///
/// The (b) grouping and the safe/live/unowned split in (c) both key on
/// whether rwv holds a persisted ownership receipt
/// ([`crate::workweave_index::RefRegistry`]) for the exact ref in the exact
/// store. A branch that merely *looks* like one of rwv's — a hand-made
/// `<a>--<b>/<c>` — is an operator branch: §7.2's first arm leaves it
/// alone, and `--fix` never deletes it.
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
        /// The ephemeral ref this workweave mints (`<project>--<workweave>`).
        expected_ref: String,
        /// The ephemeral ref rwv holds a receipt for in this repo's
        /// canonical store, when it holds one. Decides the remediation
        /// spelling: `git switch <name>` returns to an existing ref,
        /// `git switch -c` is only correct when there is none.
        recorded_ref: Option<String>,
    },
    /// (a) The workweave checkout is on an ephemeral ref rwv **recorded**
    /// for a *different* workweave. Report-only.
    ///
    /// Keyed on the receipt, not on the name (R2). Before the flat-name
    /// cutover this arm fired on any `<a>--<b>/<c>`-shaped name, which meant
    /// a hand-made branch was reported as "another workweave's" purely
    /// because of how it was spelled; now it fires only when some project's
    /// registry says the ref really is one rwv minted for another workweave.
    /// A look-alike lands in [`SharedBranch`](Self::SharedBranch) instead —
    /// both are report-only, so the distinction costs nothing but accuracy.
    ForeignEphemeral {
        /// The branch currently checked out.
        actual_branch: String,
        /// The ephemeral ref this workweave mints (`<project>--<workweave>`).
        expected_ref: String,
        /// See [`SharedBranch`](Self::SharedBranch)'s field of the same
        /// name.
        recorded_ref: Option<String>,
    },
    /// (a) The workweave checkout is in detached-HEAD state — HEAD points
    /// directly at a commit instead of a named branch. Detached HEAD
    /// breaks the merged-check and ref-namespace invariants in
    /// `clone-topology.md`.
    ///
    /// This is also `branch-model.md` §7.1's arm 3 (when `legacy_branch` is
    /// `Some`) and arm 5 (when it is `None`). `--fix
    /// --adopt-detached-checkouts` mints the workweave's flat ref **at HEAD**
    /// — i.e. at the lock SHA — and, in the arm-3 case, gives the legacy
    /// branch's name up to make room for it.
    Detached {
        /// The ephemeral ref this workweave mints (`<project>--<workweave>`).
        expected_ref: String,
        /// See [`SharedBranch`](Self::SharedBranch)'s field of the same
        /// name.
        recorded_ref: Option<String>,
        /// The commit HEAD names directly.
        at_sha: String,
        /// §7.1 arm 3: a pre-flat branch of this workweave's own namespace,
        /// with its tip. Arm 3 requires **both** tips be reported, because
        /// they are the two things the operator is choosing between.
        legacy_branch: Option<LegacyRefAtTip>,
    },
    /// (a) `branch-model.md` §7.1 arm 1: the workweave checkout is attached
    /// to a pre-flat `<project>--<workweave>/<segment>` ref of its **own**
    /// namespace.
    ///
    /// The common migration case and the fully automatic one: `--fix`
    /// records a receipt at the ref's current tip and renames it to the flat
    /// name. Nothing is lost — a rename preserves the tip — and the
    /// namespace membership is decided against the name this workweave
    /// *mints*, never by taking the observed name apart
    /// ([`LegacyEphemeralRefName`](crate::vcs::LegacyEphemeralRefName)).
    UnmigratedEphemeralBranch {
        /// The pre-flat branch currently checked out.
        actual_branch: String,
        /// The flat ref it migrates to (`<project>--<workweave>`).
        expected_ref: String,
    },
    /// (a) `branch-model.md` §7.1 arm 2: the workweave's flat ref exists in
    /// the canonical store, but rwv holds no receipt for it.
    ///
    /// The state a build that minted flat names before receipts existed
    /// leaves behind, and the state a migration crash between the receipt
    /// and the rename would leave if the receipt had not been written first.
    /// Under R2 the ref is nobody's until adopted, so `workweave delete`
    /// cannot clean it up; `--fix` adopts it at its observed tip.
    UnrecordedEphemeralBranch {
        /// The flat ref (`<project>--<workweave>`).
        branch: String,
    },
    /// (a) `branch-model.md` §7.1 arm 6: the workweave checkout is on a
    /// branch with no commits.
    ///
    /// Report-only, and not because a fix is missing: there is no revision
    /// to record a receipt against, so there is nothing the migration could
    /// own. `rwv lock` is where an unborn HEAD is actionable (§4.5).
    UnbornCheckout {
        /// The branch HEAD points at, which has no commits yet.
        branch: String,
    },
    /// (b) §7.2 arm 2: the canonical store is attached to a ref rwv
    /// recorded as belonging to a workweave that is **still on disk**.
    ///
    /// An I3 disjointness violation. git forbids one branch being checked
    /// out in two worktrees of the same store, so reaching this state means
    /// a directory was moved or copied. Report-only — there is no fix that
    /// does not guess which of the two checkouts is the real one.
    CanonicalHoldsLiveWorkweaveRef {
        /// The branch the canonical store is attached to.
        actual_branch: String,
        /// The live workweave the receipt says that ref belongs to.
        workweave_name: String,
    },
    /// (b) §7.2 arm 3: the canonical store is attached to a ref rwv
    /// recorded as belonging to a workweave that is **gone** — a leak.
    ///
    /// Report-only in practice: the DESTROY that would reclaim the ref
    /// cannot run while this store's own HEAD is on it (git refuses to
    /// delete a branch a worktree uses), so `--fix` names the ref and the
    /// `git switch` that frees it rather than attempting a delete that
    /// cannot succeed. Once the store is off the ref it is an ordinary
    /// (c) finding and `--fix` reclaims it under a warrant.
    CanonicalHoldsLeakedRef {
        /// The branch the canonical store is attached to.
        actual_branch: String,
        /// The project whose registry holds the receipt.
        ///
        /// Not the workweave: §7.3 is explicit that rwv does not try to
        /// reconstruct which workweave a stray ref belonged to. The receipt
        /// records `(store, name, created_at)`, and the workweave is
        /// recoverable only while one on disk would mint that name — which
        /// is exactly the case this variant is *not*.
        project: String,
    },
    /// (b) §7.2 arm 4: the canonical store — or the project repo (§5.1) —
    /// is in detached-HEAD state.
    ///
    /// New with the branch model: the shipped scan collapsed this into "no
    /// current branch" and produced nothing, so `git checkout --detach` in
    /// a canonical (and in `projects/<project>/`) yielded zero findings
    /// while the same action in a workweave was a violation.
    ///
    /// `--fix --reattach-checkouts` reattaches when
    /// [`reattachable`](Self::CanonicalDetached::reattachable) — the
    /// tracking declaration's local counterpart exists and its tip equals
    /// HEAD. That condition is false for the ordinary post-fetch state
    /// (stale counterpart, HEAD at the lock SHA), so the fix repairs the
    /// minority; it is not weave-wide reattachment.
    CanonicalDetached {
        /// The commit HEAD names directly.
        at_sha: String,
        /// The local counterpart of the ref this repo tracks — the
        /// manifest's `version:` for a member, the remote's declared
        /// default branch for the project repo. `None` when no tracking
        /// declaration resolves, in which case there is nothing to name as
        /// a reattach target.
        counterpart: Option<String>,
        /// Whether §7.2's reattach condition holds: `counterpart` exists as
        /// a local branch **and** its tip equals HEAD.
        reattachable: bool,
    },
    /// (c) A `<project>--<name>/...` branch in the canonical clone whose
    /// workweave `<name>` no longer exists on disk, **which rwv holds an
    /// ownership receipt for**, and whose tip is an ancestor of the primary
    /// tracking branch's tip (no unique commits). Safe-class per the
    /// shared-refs-drift doctrine — `--fix` deletes it under a
    /// [`Merged`](crate::vcs::DeletionWarrant::merged) warrant, with no
    /// information loss.
    StaleEphemeralBranchSafe {
        /// The full branch name (e.g. `foundations--feat-a`).
        branch: String,
        /// The project whose registry holds the receipt.
        ///
        /// Not the workweave, for the reason
        /// [`CanonicalHoldsLeakedRef`](Self::CanonicalHoldsLeakedRef) gives:
        /// §7.3 is explicit that rwv does not reconstruct which workweave a
        /// ref belonged to, and for this class no workweave on disk would
        /// mint the name — that is what makes it stale.
        project: String,
    },
    /// (c) A `<project>--<name>/...` branch in the canonical clone whose
    /// workweave `<name>` no longer exists on disk, which rwv holds a
    /// receipt for, but whose tip carries commits not reachable from the
    /// primary tracking branch's tip (unique work). Live-class per the
    /// shared-refs-drift doctrine — report-only; `--fix` never touches
    /// this, because no [`Merged`](crate::vcs::DeletionWarrant::merged)
    /// warrant can be established for it. The operator decides whether to
    /// land the commits, archive the branch, or delete it.
    StaleEphemeralBranchLive {
        /// The full branch name.
        branch: String,
        /// The project whose registry holds the receipt. See
        /// [`StaleEphemeralBranchSafe`](Self::StaleEphemeralBranchSafe).
        project: String,
        /// The branch tip SHA, surfaced so the operator can recover the
        /// commits before deleting (e.g. `git log <tip_sha>`).
        tip_sha: String,
    },
    /// (c) A branch shaped like one rwv minted before `branch-model.md` §3.5
    /// flattened the scheme, sitting in a canonical store, which **rwv holds
    /// no ownership receipt for** and which no workweave on disk claims.
    ///
    /// Under R2 this ref is not rwv's: name shape is not ownership. It is
    /// reported so the operator can see it, and it is never deleted — the
    /// shipped scanner deleted exactly this class, which is why a hand-made
    /// `<a>--<b>/<c>` branch could disappear under `--fix`.
    ///
    /// # Why this one is discovered by shape and nothing else is
    ///
    /// Every other arm asks the registry or asks a live workweave's
    /// **minted** name. This arm has neither to ask: there is no receipt, and
    /// §7.3 forbids reconstructing which workweave the ref belonged to — so
    /// the alternative to a shape heuristic is not a better signal, it is
    /// silence, and the refs the operator most needs to see (the pre-receipt
    /// population §7.1's migration cannot reach) would simply stop being
    /// reported.
    ///
    /// What keeps that sound is that the heuristic yields a `bool` and
    /// nothing else — see [`looks_like_a_pre_flat_ref`]. No name is taken
    /// apart, no workweave is named, and the only route to a DESTROY runs
    /// through an `OwnedRef`, which only a persisted receipt produces. A
    /// false positive costs one line of output and can cost nothing more.
    StaleEphemeralBranchUnowned {
        /// The full branch name.
        branch: String,
    },
}

/// A pre-flat branch and the commit it reaches, as `branch-model.md` §7.1
/// arm 3 requires them to be reported: **both** tips, side by side, because
/// the operator is choosing between them.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct LegacyRefAtTip {
    /// The pre-flat branch name (`<project>--<workweave>/<segment>`).
    pub branch: String,
    /// Its tip.
    pub tip_sha: String,
    /// Whether that tip carries commits the detached HEAD does not — i.e.
    /// whether adopting the checkout would **strand** work. Arm 3 makes the
    /// warning mandatory in exactly this case.
    pub strands_commits: bool,
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
//     IncompleteLock      -> "incomplete-lock"
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
    IncompleteLock {
        path: String,
        absolute_path: String,
        project: String,
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
    LegacyWorkweaveIndex {
        project: String,
        index_path: String,
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
    DanglingRefReceipt {
        /// The project whose registry holds the receipt.
        project: String,
        /// Absolute path of the canonical store the receipt is keyed to.
        store_path: String,
        /// The recorded ref name that does not exist in that store.
        ref_name: String,
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
            CheckViolation::IncompleteLock { project, repo } => Self::IncompleteLock {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.to_string(),
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
            CheckViolation::LegacyWorkweaveIndex {
                project,
                index_path,
            } => Self::LegacyWorkweaveIndex {
                project: project.to_string(),
                index_path: index_path.to_string_lossy().into_owned(),
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
            CheckViolation::DanglingRefReceipt {
                project,
                store_path,
                ref_name,
            } => Self::DanglingRefReceipt {
                project: project.to_string(),
                store_path: store_path.to_string_lossy().into_owned(),
                ref_name,
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
    let mut found = Vec::new();
    for container in workweave_containers_for_scan(ws_root) {
        let entries = match std::fs::read_dir(&container) {
            Ok(e) => e,
            Err(_) => continue,
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
    }
    found.sort_by(|a, b| a.marker_path.cmp(&b.marker_path));
    // De-duplicate: the same container may appear under multiple projects
    // (default container is shared), so the same marker can be visited twice.
    found.dedup_by(|a, b| a.marker_path == b.marker_path);
    found
}

/// Enumerate the distinct workweave containers to scan for reconciliation.
///
/// Every project records its own container in `.rwv-workweave-index`; the
/// default `<parent-of-primary>/.workweaves` is also always scanned so that
/// a workspace without any registered containers (bootstrap case for a
/// pre-registry workspace) still surfaces on-disk workweaves. Duplicates are
/// collapsed by path to avoid double-reporting.
fn workweave_containers_for_scan(ws_root: &Path) -> Vec<PathBuf> {
    let mut containers: Vec<PathBuf> = Vec::new();
    let push_unique = |p: PathBuf, containers: &mut Vec<PathBuf>| {
        let canonical = p.canonicalize().unwrap_or(p);
        if !containers.contains(&canonical) {
            containers.push(canonical);
        }
    };
    push_unique(
        crate::workweave_index::default_container(ws_root),
        &mut containers,
    );
    for project in crate::workweave_index::projects_on_disk(ws_root) {
        if let Ok(Some(idx)) = crate::workweave_index::read(ws_root, &project) {
            push_unique(idx.container, &mut containers);
        }
    }
    // Env-var fallback: for the deprecation window, include it too so a user
    // whose workspace still lives at `$RWV_WORKWEAVE_DIR` gets doctor
    // coverage. Consumption itself will go away in the follow-up bead.
    if let Ok(v) = std::env::var("RWV_WORKWEAVE_DIR") {
        if !v.is_empty() {
            push_unique(PathBuf::from(v), &mut containers);
        }
    }
    containers
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

/// Reconcile every project's `.rwv-workweave-index` against on-disk state.
///
/// Emits three registry-specific finding kinds:
///
/// * `stale-registry-entry` — a recorded name → path whose validation fails
///   (missing directory, missing / foreign / cross-project marker). Prunable
///   via `rwv doctor --fix`.
/// * `unregistered-workweave` — a marker-bearing workweave present in a
///   container that is NOT recorded in that project's index. Adoptable via
///   `rwv doctor --fix`. Silent adoption in read paths is deliberately not
///   done — adoption is an operator-consented act.
/// * `tracked-index` — the `.rwv-workweave-index` file is tracked by the
///   project repo's VCS. The design tolerates this (reads route to primary;
///   the index is advisory) but tracks it as a hygiene finding.
///
/// The scan iterates every project's recorded container so per-workweave
/// placement overrides (a workweave living outside the default container)
/// still get reconciliation coverage. Bootstrapping workspaces (no index
/// yet, live workweaves at the compiled-in default) surface every
/// marker-bearing directory as `unregistered-workweave` — the intended
/// self-heal path is `rwv doctor --fix` on first run after upgrade.
fn scan_registry_reconciliation(ws_root: &Path) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    // Pass 1 — every recorded entry that fails validation is stale. Also
    // collect the set of validated (project, canonical-path) pairs so
    // pass 2 can identify unregistered orphans without double-reporting
    // ones the operator already recorded.
    let mut recorded_valid_paths: std::collections::HashSet<(String, PathBuf)> =
        std::collections::HashSet::new();
    for project in crate::workweave_index::projects_on_disk(ws_root) {
        let index = match crate::workweave_index::read(ws_root, &project) {
            Ok(Some(idx)) => idx,
            _ => continue,
        };
        for (name, path) in &index.workweaves {
            let validation = crate::workweave::validate_registry_entry(ws_root, &project, path);
            match validation {
                crate::workweave::RegistryEntryValidation::Valid => {
                    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                    recorded_valid_paths.insert((project.as_str().to_string(), canonical));
                }
                other => {
                    let reason = match other {
                        crate::workweave::RegistryEntryValidation::MissingDirectory => {
                            "recorded directory does not exist".to_string()
                        }
                        crate::workweave::RegistryEntryValidation::MissingMarker => {
                            "recorded directory has no `.rwv-workweave` marker".to_string()
                        }
                        crate::workweave::RegistryEntryValidation::ForeignPrimary => {
                            "marker `primary` does not match this workspace".to_string()
                        }
                        crate::workweave::RegistryEntryValidation::ProjectMismatch { actual } => {
                            format!(
                                "marker records project `{}`, not `{}`",
                                actual.as_str(),
                                project.as_str()
                            )
                        }
                        crate::workweave::RegistryEntryValidation::MarkerUnreadable { detail } => {
                            format!("marker is unreadable ({detail})")
                        }
                        crate::workweave::RegistryEntryValidation::Valid => unreachable!(),
                    };
                    violations.push(CheckViolation::WorkweaveTreeIntegrity {
                        workweave_dir: path.clone(),
                        sub_kind: WorkweaveTreeIntegrityKind::StaleRegistryEntry {
                            project: project.as_str().to_string(),
                            workweave_name: name.clone(),
                            recorded_path: path.clone(),
                            reason,
                        },
                    });
                }
            }
        }

        // Tracked-index hygiene finding. Only meaningful for git-tracked
        // project repos; a non-git project has nothing to track. Silent on
        // errors (a missing repo, a non-git repo) — hygiene, not correctness.
        let index_path = crate::workweave_index::index_path(ws_root, &project);
        if index_path.exists() && is_tracked_by_git(&index_path) {
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: index_path.clone(),
                sub_kind: WorkweaveTreeIntegrityKind::TrackedIndex {
                    project: project.as_str().to_string(),
                    index_path,
                },
            });
        }
    }

    // Pass 2 — every marker-bearing workweave on disk (from any container)
    // that is not in the validated set is an orphan. Uses
    // `doctor_scan_container` for the shape-parse, iterating every unique
    // container so per-workweave overrides are covered.
    let mut seen_orphans: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for container in workweave_containers_for_scan(ws_root) {
        for (project, name, dir) in crate::workweave::doctor_scan_container(ws_root, &container) {
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let key = (project.as_str().to_string(), canonical.clone());
            if recorded_valid_paths.contains(&key) {
                continue;
            }
            if !seen_orphans.insert(canonical) {
                continue;
            }
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: dir,
                sub_kind: WorkweaveTreeIntegrityKind::UnregisteredWorkweave {
                    project: project.as_str().to_string(),
                    workweave_name: name,
                },
            });
        }
    }

    violations
}

/// Best-effort check for whether `path` is tracked by git.
///
/// Uses `git ls-files --error-unmatch <path>` from the file's parent
/// directory. Any error (not a git repo, git not installed, etc.) returns
/// `false` — hygiene surfaces should never fabricate findings on
/// non-git-managed projects.
fn is_tracked_by_git(path: &Path) -> bool {
    let dir = match path.parent() {
        Some(d) => d,
        None => return false,
    };
    let name = match path.file_name() {
        Some(n) => n,
        None => return false,
    };
    let output = std::process::Command::new("git")
        .args(["ls-files", "--error-unmatch"])
        .arg(name)
        .current_dir(dir)
        .output();
    matches!(output, Ok(o) if o.status.success())
}

/// Adopt an on-disk workweave into its project's `.rwv-workweave-index`.
///
/// Called by `rwv doctor --fix` for each `UnregisteredWorkweave` finding.
/// Idempotent: a race with a concurrent `workweave create` recording the
/// same entry is harmless (the writer replaces the entry with an
/// identical value).
pub fn fix_unregistered_workweave(
    ws_root: &Path,
    project: &crate::manifest::ProjectName,
    workweave_name: &str,
    workweave_dir: &Path,
) -> anyhow::Result<()> {
    let canonical = workweave_dir
        .canonicalize()
        .unwrap_or_else(|_| workweave_dir.to_path_buf());
    crate::workweave_index::record_workweave(ws_root, project, workweave_name, canonical)
}

/// Prune a stale entry from a project's `.rwv-workweave-index`.
///
/// Called by `rwv doctor --fix` for each `StaleRegistryEntry` finding.
/// Idempotent.
pub fn fix_stale_registry_entry(
    ws_root: &Path,
    project: &crate::manifest::ProjectName,
    workweave_name: &str,
) -> anyhow::Result<()> {
    crate::workweave_index::forget_workweave(ws_root, project, workweave_name)
}

/// Scan the workweave parent directories for `.rwv-workweave` marker tree
/// anomalies.
///
/// Checks performed:
///
/// 1. **`dangling-parent`** — marker's `parent:` path does not exist on disk.
///    Auto-fixable via `rwv doctor --fix` (re-points to primary); the other
///    three shape sub-kinds are report-only.
/// 2. **`parent-chain-anomaly`** — cycle (A→B→A…), parent==self, or the
///    parent marker's `project` differs from the child's `project`.
/// 3. **`unregistered-dir`** — a directory under a workweave container that
///    has no `.rwv-workweave` marker file.
/// 4. **`foreign-primary`** — marker's `primary:` does not canonicalize to
///    `ws_root`.
/// 5. **`stale-registry-entry`** — a `.rwv-workweave-index` entry whose
///    recorded path fails marker round-trip validation. Auto-fixable
///    (`--fix` prunes the entry).
/// 6. **`unregistered-workweave`** — a marker-bearing workweave present on
///    disk but absent from its project's index. Auto-fixable (`--fix`
///    adopts the entry). This is also the migration surface for
///    workspaces that predate the registry: run `rwv doctor --fix` to
///    self-heal the index from the on-disk workweaves.
/// 7. **`tracked-index`** — the machine-local `.rwv-workweave-index` file
///    is tracked by the project's VCS. Report-only hygiene finding.
///
/// Workweave containers are enumerated per project (every recorded
/// container, plus the compiled-in default), so per-workweave placement
/// overrides get coverage.
pub fn scan_workweave_tree_integrity(ws_root: &Path) -> Vec<CheckViolation> {
    let ws_canonical = ws_root
        .canonicalize()
        .unwrap_or_else(|_| ws_root.to_path_buf());

    let mut violations = Vec::new();

    // Emit registry-vs-disk reconciliation findings first: stale entries
    // (registered but not a valid workweave on disk), unregistered
    // workweaves (present on disk but not in the registry), and tracked
    // indexes (index file committed to the project repo). These are the
    // findings the registry-based design added; the marker-integrity
    // checks that follow are unchanged from the pre-registry era except
    // that they now iterate over every recorded container.
    violations.extend(scan_registry_reconciliation(ws_root));

    // Enumerate every unique container to scan for marker-shape issues.
    let containers = workweave_containers_for_scan(ws_root);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for container in &containers {
        let entries = match std::fs::read_dir(container) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_dir() {
                continue;
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if seen.insert(canonical) {
                dirs.push(path);
            }
        }
    }
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
// checkout sits on a `<project>--<workweave>` ephemeral branch owned by
// exactly that workweave; canonicals sit on a non-ephemeral branch; and
// stale ephemeral branches left in canonicals by crashed deletes are
// surfaced under the safe/live doctrine from
// `docs/explanation/joints/shared-refs-drift.md`.
//
// VCS seam: the scanner consumes the `Vcs` trait — `observe_head`,
// `head_attachment`, `list_local_branches`, `head_revision`,
// `resolve_revision`, and `is_ancestor` — without any git-specific code.
// See `docs/explanation/joints/vcs-as-seam.md`.

/// Whether `name` is worth **showing** the operator as a possible leftover of
/// the pre-§3.5 naming scheme.
///
/// This is the successor of `parse_ephemeral_branch_name`, and the difference
/// between them is the whole point of the cutover: the parser returned the
/// components, and its callers fed those components into staleness decisions
/// and into the workweave name a finding reported — deriving ownership and
/// lineage from a string. This returns a `bool`. Nothing downstream of it can
/// learn which project or which workweave a name mentions, because it does
/// not say; and the only route to a DESTROY runs through an `OwnedRef`, which
/// only a persisted receipt produces (R2). So a false positive here costs one
/// line of report and can cost nothing more.
///
/// It is used for exactly one finding —
/// [`StaleEphemeralBranchUnowned`](BranchDisciplineKind::StaleEphemeralBranchUnowned)
/// — and that variant's doc comment records why no better signal exists for
/// that class.
///
/// The flat shape is deliberately **not** matched: a flat `<a>--<b>` with no
/// receipt is indistinguishable from an operator branch, and the ones that
/// really are rwv's are reached through a live workweave's *minted* name
/// (§7.1 arm 2), not through a guess.
fn looks_like_a_pre_flat_ref(name: &str) -> bool {
    match name.split_once('/') {
        Some((lhs, segment)) => {
            !segment.is_empty()
                && lhs.split_once("--").is_some_and(|(project, workweave)| {
                    !project.is_empty() && !workweave.is_empty()
                })
        }
        None => false,
    }
}

/// The repos of one workweave that the `branch-model.md` §7.1 pass visits,
/// each paired with the canonical store its receipts key to.
///
/// §7.1: the enumeration covers every worktree-materialized repo (skipping
/// [`ReferenceAlias`] checkouts, §5.2) **and the project-repo checkout**,
/// which the member walker does not reach — the shipped delete handles it as
/// a separate arm for the same reason, and an implementer who reuses the
/// member walker alone leaks one project-repo branch per workweave.
///
/// [`ReferenceAlias`]: crate::workweave::CheckoutKind::ReferenceAlias
fn workweave_checkouts(
    vcs: &dyn crate::vcs::Vcs,
    workweave_dir: &Path,
    project_name: &str,
) -> Vec<PathBuf> {
    use crate::workweave::{classify_checkout, CheckoutKind};

    let registries = crate::registry::builtin_registries();
    let mut out: Vec<PathBuf> =
        crate::workspace::scan_repos_on_disk(workweave_dir, &registries, vcs)
            .into_iter()
            .map(|repo| workweave_dir.join(repo.as_path()))
            .collect();
    out.push(workweave_dir.join("projects").join(project_name));
    out.retain(|abs| abs.is_dir() && classify_checkout(abs) != CheckoutKind::ReferenceAlias);
    out
}

/// The refs of this workweave's own namespace that exist in `store`,
/// **attached or not**.
///
/// §7.1's pass rule: "the pass enumerates refs per store — attached and
/// unattached — not attachment states", because a pass keyed on
/// `head_attachment` alone silently disowns a commit-bearing legacy branch
/// that a fetch left behind.
///
/// Membership is decided against the name this workweave **mints**, so the
/// listing can never reach into another workweave's namespace however the
/// refs are spelled. Returns `(flat_is_present, legacy_refs)`.
fn refs_in_workweave_namespace(
    vcs: &dyn crate::vcs::Vcs,
    store: &Path,
    flat: &crate::vcs::EphemeralRefName,
) -> (bool, Vec<crate::vcs::LegacyEphemeralRefName>) {
    // The prefix comes from `to_raw`, the named conversion to the parse
    // boundary — not from `Display`. A rendering is for a reader; what the
    // VCS is asked to match on is a name (branch-model.md §4.2).
    let flat_raw = flat.to_raw();
    let Ok(observed) = vcs.list_branch_names_with_prefix(store, flat_raw.as_str()) else {
        return (false, Vec::new());
    };
    let mut flat_present = false;
    let mut legacy = Vec::new();
    for name in observed {
        if name == flat_raw {
            flat_present = true;
        } else if let Some(claimed) = crate::vcs::LegacyEphemeralRefName::claim(flat, &name) {
            legacy.push(claimed);
        }
    }
    (flat_present, legacy)
}

// ---------------------------------------------------------------------------
// Ownership receipts, as the branch-discipline scan consults them (R2)
// ---------------------------------------------------------------------------

/// One ownership receipt as the scan uses it: the receipt itself, plus the
/// answer §7.2's arms 2 and 3 split on — whether the workweave that ref was
/// minted for is still on disk.
struct RecordedRef {
    /// The project whose registry holds the receipt.
    project: ProjectName,
    /// The receipt. Carries its store, so a receipt can never authorize a
    /// delete in a different refdb.
    owned: crate::vcs::OwnedRef,
    /// The live workweave whose minted name this receipt carries. `None`
    /// means no workweave on disk would mint it — the ref is a leak from a
    /// deleted workweave (or from a `--dir` placement the container scan
    /// cannot see, which is Q10 and stays open; that is why `None` alone
    /// never authorizes anything, only a warrant does).
    live_workweave: Option<String>,
}

/// One project's receipts, pre-filtered so per-store lookups can skip
/// projects that have none.
struct RecordedProject {
    name: ProjectName,
    /// The ref names that live workweaves of this project would mint.
    ///
    /// Membership is by **minted** name: [`EphemeralRefName::mint`] is total
    /// on `(project, workweave)`, so "which live workweave would have minted
    /// this receipt's name" is a lookup against names rwv itself produced,
    /// never a parse of a name back into its parts. R2 rules the second one
    /// out, and Q12 (the legal grammar for project and workweave names)
    /// makes it unsound anyway.
    ///
    /// [`EphemeralRefName::mint`]: crate::vcs::EphemeralRefName::mint
    live_ref_names: std::collections::HashMap<crate::vcs::RawRefName, String>,
}

/// The weave's ownership receipts (`branch-model.md` §4.2), arranged for the
/// question every arm of the branch-discipline scan now asks first: **is
/// this ref rwv's?**
///
/// R2 makes that a matter of record. A branch that merely looks like one of
/// rwv's — a hand-made `<a>--<b>/<c>` — is an operator branch, and the whole
/// point of building this view is that the scan can no longer answer the
/// ownership question by looking at the name.
///
/// Every project on disk contributes, because a canonical store is shared
/// across projects: a ref recorded by one project's registry is rwv's
/// however the scan reached the store.
struct RecordedRefs {
    ws_root: PathBuf,
    /// Projects with at least one receipt. Projects whose registry is
    /// empty, absent, or legacy are dropped at construction so the per-store
    /// lookups never re-read their index file — which keeps this view free
    /// on the workspaces that have not created an ephemeral ref yet.
    projects: Vec<RecordedProject>,
}

impl RecordedRefs {
    /// Build the view for the weave rooted at `ws_root`.
    fn new(ws_root: &Path) -> Self {
        use crate::vcs::EphemeralRefName;
        use crate::workweave_index::RefRegistry;

        let mut projects = Vec::new();
        // The container walk is hoisted: it is the expensive part, and it
        // does not vary by project. It is also skipped entirely on a weave
        // with no receipts, which is every weave until §7.1's migration
        // runs.
        let mut workweave_dirs: Option<Vec<(String, PathBuf)>> = None;
        for name in crate::workweave_index::projects_on_disk(ws_root) {
            let registry = RefRegistry::for_project(ws_root, &name);
            // A legacy index reads as "no receipts", which is the
            // fail-closed direction: nothing in it is destroyable until
            // §7.1's migration adopts it.
            match registry.list_all() {
                Ok(all) if all.is_empty() => continue,
                Ok(_) => {}
                Err(_) => continue,
            }
            let dirs = workweave_dirs
                .get_or_insert_with(|| crate::workweave::list_workweave_dirs(ws_root));
            let mut live_ref_names = std::collections::HashMap::new();
            for workweave in live_workweave_names(ws_root, &name, dirs) {
                let minted =
                    EphemeralRefName::mint(&name, &crate::manifest::WorkweaveName::new(&workweave));
                live_ref_names.insert(minted.to_raw(), workweave);
            }
            projects.push(RecordedProject {
                name,
                live_ref_names,
            });
        }
        Self {
            ws_root: ws_root.to_path_buf(),
            projects,
        }
    }

    /// Every receipt keyed to `store`, across projects.
    ///
    /// Goes through [`RefRegistry::list_for_store`] rather than matching
    /// paths here, so the store-key normalisation the registry recorded
    /// under is the one the query uses.
    ///
    /// **Re-reads the index on every call, deliberately.** That is one small
    /// file read per (project-with-receipts, store), and the alternative is a
    /// snapshot: the `--fix` paths call this again immediately before a
    /// DESTROY, and answering that call from a cache would authorize a delete
    /// against a receipt a sibling workweave had already retracted. The
    /// registry's own doc comment makes the same call for the same reason.
    /// Projects with no receipts were dropped at construction, so a weave
    /// that has never recorded one pays nothing here.
    ///
    /// [`RefRegistry::list_for_store`]: crate::workweave_index::RefRegistry::list_for_store
    fn for_store(&self, store: &Path) -> Vec<RecordedRef> {
        use crate::workweave_index::RefRegistry;

        let mut out = Vec::new();
        for project in &self.projects {
            let registry = RefRegistry::for_project(&self.ws_root, &project.name);
            let Ok(owned_refs) = registry.list_for_store(store) else {
                continue;
            };
            for owned in owned_refs {
                let live_workweave = project.live_ref_names.get(owned.name()).cloned();
                out.push(RecordedRef {
                    project: project.name.clone(),
                    owned,
                    live_workweave,
                });
            }
        }
        out
    }
}

/// The names of `project`'s workweaves that are still on disk.
///
/// Two sources, unioned, because either alone under-reports and
/// under-reporting liveness is the direction that turns a live workweave's
/// ref into a "leak":
///
/// - `workweave_dirs`, the container scan
///   ([`crate::workweave::list_workweave_dirs`]), which sees marker-carrying
///   directories under every recorded container but **not** a `--dir`
///   placement outside them (Q10, `branch-model.md` §8);
/// - the workweave index, which records `--dir` placements by absolute path
///   — consulted only for entries whose recorded directory actually exists,
///   so a stale index entry does not resurrect a deleted workweave.
fn live_workweave_names(
    ws_root: &Path,
    project: &ProjectName,
    workweave_dirs: &[(String, PathBuf)],
) -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();

    for (name, dir) in workweave_dirs {
        if let Ok(Some(marker)) = crate::workspace::WorkweaveMarker::read(dir) {
            if marker.project.as_str() == project.as_str() {
                names.insert(name.clone());
            }
        }
    }

    if let Ok(Some(index)) = crate::workweave_index::read(ws_root, project) {
        for (name, path) in &index.workweaves {
            if path.is_dir() {
                names.insert(name.clone());
            }
        }
    }

    names.into_iter().collect()
}

/// Scan a workweave's repo checkouts for (a) workweave-branch violations and
/// for `branch-model.md` §7.1's migration arms.
///
/// One pass, because it is one question asked of one place: what refs does
/// this workweave's namespace hold in each store, and what is each checkout
/// attached to. §7.1's pass rule is explicit that both halves are needed —
/// "the pass enumerates refs per store — attached and unattached — not
/// attachment states" — because a legacy branch a fetch left behind is
/// invisible to a HEAD read, and a checkout on `main` is invisible to a ref
/// listing.
///
/// The healthy state is the **minted** name, flat (§3.5): `<project>--<workweave>`,
/// no segment. The arms, and the sub-kind each produces:
///
///   * on the minted ref — healthy, nothing reported.
///   * on a pre-flat ref of this workweave's own namespace (§7.1 arm 1) —
///     [`UnmigratedEphemeralBranch`], which `--fix` renames.
///   * on a ref some project **recorded** for another workweave —
///     [`ForeignEphemeral`] (§7.1 arm 4).
///   * on any other branch — [`SharedBranch`] (§7.1 arm 4); covers the
///     bare-main-in-workweave case from the spec's acceptance criteria.
///   * detached (§7.1 arms 3 and 5) — [`Detached`], carrying the pre-flat
///     branch and **both** tips when one exists, which is what arm 3 requires
///     the report to show.
///   * unborn (§7.1 arm 6) — [`UnbornCheckout`]. Report-only: there is no
///     revision to record a receipt against.
///   * independently of the attachment: the minted ref present with no
///     receipt (§7.1 arm 2) — [`UnrecordedEphemeralBranch`], which `--fix`
///     adopts.
///
/// Nothing here parses a branch name. "Is this ref mine" goes through
/// [`EphemeralRefName::mint`] and [`LegacyEphemeralRefName::claim`], both of
/// which start from the name this workweave *would* produce; "is this ref
/// rwv's" goes through the receipt registry (R2).
///
/// The remediation each finding carries is **registry-aware**: when rwv
/// holds a receipt for this workweave's ref in the repo's canonical store,
/// the advice is `git switch <name>` — returning to a branch that exists.
/// `git switch -c` is printed only when there is no recorded ref to return
/// to. The distinction is not cosmetic: `git switch <name>` on an absent
/// branch does not refuse, it invents the branch from a remote-tracking ref
/// of the same name, or detaches when the name is a tag's, or reads the name
/// as a pathspec and reverts the operator's uncommitted edits to it — all
/// exiting 0.
///
/// [`UnmigratedEphemeralBranch`]: BranchDisciplineKind::UnmigratedEphemeralBranch
/// [`UnrecordedEphemeralBranch`]: BranchDisciplineKind::UnrecordedEphemeralBranch
/// [`ForeignEphemeral`]: BranchDisciplineKind::ForeignEphemeral
/// [`SharedBranch`]: BranchDisciplineKind::SharedBranch
/// [`Detached`]: BranchDisciplineKind::Detached
/// [`UnbornCheckout`]: BranchDisciplineKind::UnbornCheckout
/// [`EphemeralRefName::mint`]: crate::vcs::EphemeralRefName::mint
/// [`LegacyEphemeralRefName::claim`]: crate::vcs::LegacyEphemeralRefName::claim
fn scan_workweave_repo_branches(
    vcs: &dyn crate::vcs::Vcs,
    _ws_root: &Path,
    recorded: &RecordedRefs,
    workweave_dir: &Path,
    project_name: &str,
    workweave_name: &str,
    out: &mut Vec<CheckViolation>,
) {
    use crate::vcs::{EphemeralRefName, HeadAttachment};

    let flat = EphemeralRefName::mint(
        &ProjectName::new(project_name),
        &crate::manifest::WorkweaveName::new(workweave_name),
    );
    let expected_ref = flat.to_string();

    for abs in workweave_checkouts(vcs, workweave_dir, project_name) {
        // The receipt, if any, lives in this checkout's canonical store —
        // resolved from the checkout itself rather than assembled from the
        // primary and a manifest path, so an inverted topology reports
        // against the store the refs are actually in. Resolved per repo
        // because a receipt is keyed by (store, name).
        let store = crate::workweave::receipt_store_for(&abs);
        let store_receipts = recorded.for_store(&store);
        let flat_raw = flat.to_raw();
        let recorded_ref = store_receipts
            .iter()
            .find(|rec| rec.owned.name() == &flat_raw)
            .map(|rec| rec.owned.to_string());

        let (flat_present, legacy_refs) = refs_in_workweave_namespace(vcs, &store, &flat);

        // §7.1 arm 2, and it is asked of the *store*, not of the attachment:
        // the flat ref can exist with no receipt while HEAD sits somewhere
        // else entirely, and that ref is exactly the one R2 would otherwise
        // disown forever.
        if flat_present && recorded_ref.is_none() {
            out.push(CheckViolation::BranchDiscipline {
                repo_path: abs.clone(),
                sub_kind: BranchDisciplineKind::UnrecordedEphemeralBranch {
                    branch: expected_ref.clone(),
                },
            });
        }

        let sub_kind = match vcs.head_attachment(&abs) {
            Ok(HeadAttachment::Attached(a)) => {
                if a.is_minted(&flat) {
                    continue; // healthy
                }
                match a.legacy_name_under(&flat) {
                    // §7.1 arm 1.
                    Some(legacy) => BranchDisciplineKind::UnmigratedEphemeralBranch {
                        actual_branch: legacy.to_string(),
                        expected_ref: expected_ref.clone(),
                    },
                    // §7.1 arm 4. Foreign vs shared is decided by the
                    // registry (R2): a ref some project recorded for a
                    // different workweave really is another workweave's; a
                    // look-alike is the operator's, and saying so is the
                    // whole content of the rule.
                    None if store_receipts
                        .iter()
                        .any(|rec| rec.owned.is_attached_by(&a)) =>
                    {
                        BranchDisciplineKind::ForeignEphemeral {
                            actual_branch: a.to_string(),
                            expected_ref: expected_ref.clone(),
                            recorded_ref: recorded_ref.clone(),
                        }
                    }
                    None => BranchDisciplineKind::SharedBranch {
                        actual_branch: a.to_string(),
                        expected_ref: expected_ref.clone(),
                        recorded_ref: recorded_ref.clone(),
                    },
                }
            }
            // §7.1 arms 3 and 5.
            Ok(HeadAttachment::Detached(d)) => BranchDisciplineKind::Detached {
                expected_ref: expected_ref.clone(),
                recorded_ref: recorded_ref.clone(),
                at_sha: d.at().as_str().to_string(),
                legacy_branch: legacy_refs
                    .first()
                    .map(|legacy| legacy_ref_at_tip(vcs, &store, legacy, d.at())),
            },
            // §7.1 arm 6.
            Ok(HeadAttachment::Unborn(u)) => BranchDisciplineKind::UnbornCheckout {
                branch: u.name().as_str().to_string(),
            },
            // Treat read failures as best-effort silence (matches existing
            // doctor patterns for transient git errors).
            Err(_) => continue,
        };
        out.push(CheckViolation::BranchDiscipline {
            repo_path: abs,
            sub_kind,
        });
    }
}

/// Read a pre-flat branch's tip and decide whether adopting the checkout at
/// `head` would strand it — `branch-model.md` §7.1 arm 3's "**must** warn"
/// condition.
///
/// Structural, per §7.2: the question is ancestry, never how long ago
/// anything happened. A tip that is an ancestor of `head` is carried by the
/// commit HEAD already names, so the branch's name can go without any commit
/// going with it; anything else strands work, and the report has to say so.
///
/// Unreadable ancestry counts as stranding. The direction that is wrong to
/// guess is the quiet one.
fn legacy_ref_at_tip(
    vcs: &dyn crate::vcs::Vcs,
    store: &Path,
    legacy: &crate::vcs::LegacyEphemeralRefName,
    head: &crate::vcs::ResolvedRevisionId,
) -> LegacyRefAtTip {
    let raw = legacy.to_raw();
    let tip = vcs.resolve_local_branch_tip(store, &raw).ok().flatten();
    let strands_commits = match &tip {
        Some(t) => !vcs.is_ancestor(store, t, head).unwrap_or(false),
        None => false,
    };
    LegacyRefAtTip {
        branch: legacy.to_string(),
        tip_sha: tip.map(|t| t.as_str().to_string()).unwrap_or_default(),
        strands_commits,
    }
}

/// Where a canonical store's tracking declaration comes from.
///
/// The two flavours mirror the two publish gates in `push.rs` (§4.6 (2)):
/// a manifest member's counterpart is the local projection of its declared
/// `version:`, the project repo's is the local projection of the remote's
/// declared default branch. §5.1 decided the project repo *is* an instance
/// of the branch model, so it gets an arm here rather than an exemption.
enum TrackingSource {
    /// A manifest member with exactly one declared `version:` across the
    /// projects that reference it.
    Declared(crate::vcs::TrackingRef),
    /// The project repo. Its counterpart is observed, not declared: Q6 (what
    /// a channel's publish ref is) stays open, and reading the remote's own
    /// HEAD answers "which branch is this repo's trunk" without deciding it.
    RemoteDefault,
    /// No declaration resolves — the repo is on disk but in no manifest, or
    /// two projects declare different `version:` values for it. Nothing can
    /// be named as a reattach target, so §7.2's Detached arm reports only.
    Unresolvable,
}

/// One canonical store the §7.2 pass visits.
struct CanonicalStore {
    /// Absolute path of the store.
    path: PathBuf,
    tracking: TrackingSource,
}

impl CanonicalStore {
    /// The local branch a detached HEAD here would reattach to, per §7.2.
    ///
    /// Re-derived (never cached from the scan) at every use, including the
    /// `--fix` path: the counterpart is a projection of state that can
    /// change between report and repair.
    fn local_counterpart(&self, vcs: &dyn crate::vcs::Vcs) -> Option<crate::vcs::LocalRefName> {
        match &self.tracking {
            TrackingSource::Declared(t) => Some(t.local_counterpart()),
            TrackingSource::RemoteDefault => vcs
                .remote_default_branch(&self.path)
                .ok()
                .flatten()
                .map(|d| d.local_counterpart()),
            TrackingSource::Unresolvable => None,
        }
    }
}

/// Enumerate the canonical stores under `ws_root`, with the tracking
/// declaration each one's counterpart is projected from.
///
/// Two sources, kept separate on purpose. Manifest members come from
/// [`crate::workspace::scan_repos_on_disk`], which walks the registry
/// directories. `projects/<project>/` does **not** — §5.1 is explicit that
/// the scan there is by workspace, not by registry directory, so the project
/// directory is enumerated on its own, the way `create` and `sync` already
/// do it. Reusing the registry walker would keep the hole it left: today
/// `git checkout --detach` in `projects/<project>/` yields zero findings
/// while the same action on a member is a violation.
fn canonical_stores(
    vcs: &dyn crate::vcs::Vcs,
    ws_root: &Path,
    projects: &[Project],
) -> Vec<CanonicalStore> {
    use crate::vcs::{RawRefName, TrackingRef};

    // One repo may be declared by several projects. Collect the distinct
    // declarations and only project a counterpart when they agree —
    // disagreement is a manifest question, not something to pick a winner
    // for inside a scan that may then MOVE the ref.
    let mut declared: BTreeMap<RepoPath, BTreeSet<String>> = BTreeMap::new();
    for project in projects {
        for (repo_path, entry) in project.manifest.iter_entries() {
            declared
                .entry(repo_path.clone())
                .or_default()
                .insert(entry.version.as_str().to_owned());
        }
    }

    let mut out = Vec::new();

    let registries = crate::registry::builtin_registries();
    for repo in crate::workspace::scan_repos_on_disk(ws_root, &registries, vcs) {
        let tracking = match declared.get(&repo) {
            Some(versions) if versions.len() == 1 => {
                let raw = RawRefName::new(versions.iter().next().expect("len == 1").clone());
                match TrackingRef::parse(raw) {
                    Ok(t) => TrackingSource::Declared(t),
                    // A `version:` that is not a usable tracking declaration
                    // (sha-shaped, tag-shaped) names no local counterpart.
                    Err(_) => TrackingSource::Unresolvable,
                }
            }
            _ => TrackingSource::Unresolvable,
        };
        out.push(CanonicalStore {
            path: ws_root.join(repo.as_path()),
            tracking,
        });
    }

    for project in crate::workweave_index::projects_on_disk(ws_root) {
        let path = ws_root.join("projects").join(project.as_str());
        // "Not a repo" is a typed error now, not a state (§4.5), so the
        // enumeration can ask the question directly instead of guessing from
        // a collapsed `None`.
        if !vcs.is_repo(&path) {
            continue;
        }
        out.push(CanonicalStore {
            path,
            tracking: TrackingSource::RemoteDefault,
        });
    }

    out
}

/// Scan every canonical store under `ws_root` — manifest members and, per
/// §5.1, `projects/<project>/` — for the `branch-model.md` §7.2 arms plus
/// (c) stale-ephemeral-branches.
///
/// §7.2, in order:
///
///   * `Attached(a)` to a ref rwv holds **no** receipt for — leave it alone.
///     The canonical's attachment is operator state. This is where a
///     hand-made `<a>--<b>/<c>` branch now lands: ownership is by record
///     (R2), so a name that merely looks like rwv's is the operator's.
///   * `Attached(a)` to a ref recorded to a **live** workweave —
///     [`CanonicalHoldsLiveWorkweaveRef`]. git forbids the topology, so a
///     directory was moved or copied. Report; no automatic fix.
///   * `Attached(a)` to a ref recorded to a **deleted** workweave —
///     [`CanonicalHoldsLeakedRef`]. Report; see that variant for why the
///     reclamation cannot run while this store's HEAD is on the ref.
///   * `Unborn(_)` — no arm. There is no ref to own yet and nothing to
///     reattach; a freshly `init`ed canonical is a legal state, and `lock`
///     is where the unborn HEAD is reported (§4.5).
///   * `Detached(_)` — [`CanonicalDetached`], a finding that produced
///     nothing before the model.
///
/// (c) leaked ephemeral refs. **Ranges over the store's receipts**, not over
/// its branch listing: "this ref belongs to a workweave that is gone" is a
/// question about the record, and the branch listing cannot answer it without
/// taking a name apart — which is what R2 and §7.3 forbid, and what the
/// flat-name cutover removed the machinery for. A receipt whose ref still
/// exists and whose workweave no longer does is split two ways: safe (the tip
/// is an ancestor of the store's tip, so a [`Merged`] warrant can be
/// established) and live (the tip carries commits the store's tip does not).
///
/// One class survives outside the record —
/// [`StaleEphemeralBranchUnowned`]: a pre-flat-shaped branch that no receipt
/// names and no live workweave's namespace claims. It is discovered by shape
/// because nothing else can see it at all, it is reported and never touched,
/// and [`looks_like_a_pre_flat_ref`] carries the argument for why that is
/// sound.
///
/// [`StaleEphemeralBranchUnowned`]: BranchDisciplineKind::StaleEphemeralBranchUnowned
///
/// [`CanonicalHoldsLiveWorkweaveRef`]: BranchDisciplineKind::CanonicalHoldsLiveWorkweaveRef
/// [`CanonicalHoldsLeakedRef`]: BranchDisciplineKind::CanonicalHoldsLeakedRef
/// [`CanonicalDetached`]: BranchDisciplineKind::CanonicalDetached
/// [`Merged`]: crate::vcs::DeletionWarrant::merged
fn scan_canonical_stores(
    vcs: &dyn crate::vcs::Vcs,
    ws_root: &Path,
    projects: &[Project],
    recorded: &RecordedRefs,
    out: &mut Vec<CheckViolation>,
) {
    use crate::vcs::{HeadAttachment, RawRefName};

    // Every ref name a workweave on disk would mint, across projects. Used
    // by (c)'s unowned arm to stay out of the migration's way: a pre-flat ref
    // inside a *live* workweave's namespace is §7.1 arm 1's business and is
    // already reported there, with a fix attached.
    let live_namespaces = live_minted_ref_names(ws_root);

    for store in canonical_stores(vcs, ws_root, projects) {
        let abs = &store.path;
        let store_receipts = recorded.for_store(abs);

        // §7.2's arms. The match is exhaustive over the three states
        // `head_attachment` is total on, which is what makes the Detached
        // arm impossible to leave out — the shipped scan read a collapsed
        // `Option` and simply had no branch for it.
        match vcs.head_attachment(abs) {
            Ok(HeadAttachment::Attached(a)) => {
                // Ownership by record: ask each receipt keyed to this store
                // whether the checkout is on it. `is_attached_by` is a named
                // predicate over the receipt and the witness — no name is
                // spelled, and no name shape is consulted.
                if let Some(rec) = store_receipts.iter().find(|r| r.owned.is_attached_by(&a)) {
                    let sub_kind = match &rec.live_workweave {
                        Some(workweave_name) => {
                            BranchDisciplineKind::CanonicalHoldsLiveWorkweaveRef {
                                actual_branch: rec.owned.to_string(),
                                workweave_name: workweave_name.clone(),
                            }
                        }
                        None => BranchDisciplineKind::CanonicalHoldsLeakedRef {
                            actual_branch: rec.owned.to_string(),
                            project: rec.project.to_string(),
                        },
                    };
                    out.push(CheckViolation::BranchDiscipline {
                        repo_path: abs.clone(),
                        sub_kind,
                    });
                }
                // No receipt → arm 1: operator state, left alone.
            }
            Ok(HeadAttachment::Unborn(_)) => {}
            Ok(HeadAttachment::Detached(d)) => {
                let counterpart = store.local_counterpart(vcs);
                let reattachable = match &counterpart {
                    Some(name) => {
                        // §7.2's condition, both halves: the counterpart
                        // must exist as a LOCAL branch, and its tip must
                        // equal HEAD. Resolved in the local-branch namespace
                        // so a tag of the same name cannot answer instead.
                        matches!(
                            vcs.resolve_local_branch_tip(abs, &RawRefName::new(name.as_str())),
                            Ok(Some(ref tip)) if tip == d.at()
                        )
                    }
                    None => false,
                };
                out.push(CheckViolation::BranchDiscipline {
                    repo_path: abs.clone(),
                    sub_kind: BranchDisciplineKind::CanonicalDetached {
                        at_sha: d.at().as_str().to_string(),
                        counterpart: counterpart.map(|c| c.to_string()),
                        reattachable,
                    },
                });
            }
            // Not a repo / unreadable ref database. Both are typed errors
            // rather than states; doctor stays best-effort silent on them,
            // matching how it treats every other transient VCS failure.
            Err(_) => continue,
        }

        // (c) leaked ephemeral refs, over the receipts keyed to this store.
        //
        // Cache the store's tip so per-ref classification shares one
        // `head_revision` call.
        let primary_tip = vcs.head_revision(abs).ok();

        for rec in &store_receipts {
            // A receipt whose workweave is still on disk is not leaked at
            // all. The receipt is the authority when the container scan
            // disagrees — a `--dir` placement outside every container is
            // invisible to that scan (Q10), and treating its live ref as a
            // leak is the exact failure §7.3 exists to prevent.
            if rec.live_workweave.is_some() {
                continue;
            }
            // The ref may be gone: that is the dangling-receipt state, which
            // `scan_dangling_receipts` owns. Reporting it here as well would
            // give one condition two findings and two `--fix` paths.
            if !matches!(
                vcs.resolve_local_branch_tip(abs, rec.owned.name()),
                Ok(Some(_))
            ) {
                continue;
            }

            // Classify by warrant. `merged` runs the ancestry check it
            // certifies, so the classification the report shows and the
            // authorization `--fix` needs are the same question asked of the
            // same primitive. No readable tip (unborn / corrupt store) means
            // no baseline, so no warrant, so live class — `--fix` will not
            // touch it.
            let merged = primary_tip.as_ref().is_some_and(|primary| {
                crate::vcs::DeletionWarrant::merged(vcs, &rec.owned, primary).is_some()
            });
            let sub_kind = if merged {
                BranchDisciplineKind::StaleEphemeralBranchSafe {
                    branch: rec.owned.to_string(),
                    project: rec.project.to_string(),
                }
            } else {
                let tip_sha = vcs
                    .resolve_local_branch_tip(abs, rec.owned.name())
                    .ok()
                    .flatten()
                    .map(|t| t.as_str().to_string())
                    .unwrap_or_default();
                BranchDisciplineKind::StaleEphemeralBranchLive {
                    branch: rec.owned.to_string(),
                    project: rec.project.to_string(),
                    tip_sha,
                }
            };
            out.push(CheckViolation::BranchDiscipline {
                repo_path: abs.clone(),
                sub_kind,
            });
        }

        // (c) continued: the pre-receipt population. A branch shaped like one
        // rwv minted before §3.5, that no receipt names and that no live
        // workweave's namespace claims. Report-only, forever: under R2 it is
        // not rwv's, and §7.3 forbids reconstructing whose it was.
        let Ok(branches) = vcs.list_local_branch_names(abs) else {
            continue;
        };
        for name in &branches {
            if !looks_like_a_pre_flat_ref(name.as_str()) {
                continue;
            }
            if store_receipts.iter().any(|r| r.owned.name() == name) {
                continue;
            }
            // §7.1 arm 1's territory: a live workweave still claims this
            // namespace, so the migration can rename it and the (a) pass
            // already said so.
            if live_namespaces
                .iter()
                .any(|flat| crate::vcs::LegacyEphemeralRefName::claim(flat, name).is_some())
            {
                continue;
            }
            out.push(CheckViolation::BranchDiscipline {
                repo_path: abs.clone(),
                sub_kind: BranchDisciplineKind::StaleEphemeralBranchUnowned {
                    branch: name.as_str().to_string(),
                },
            });
        }
    }
}

/// Every ephemeral ref name a workweave **on disk** would mint, across every
/// project of the weave.
///
/// Minted, never parsed: the set is built from `(project, workweave)` pairs
/// the container scan and the workweave indexes report, so membership is
/// "rwv's own naming scheme would produce this" rather than "this name looks
/// like one of rwv's".
fn live_minted_ref_names(ws_root: &Path) -> Vec<crate::vcs::EphemeralRefName> {
    use crate::vcs::EphemeralRefName;

    let mut out = Vec::new();
    // The container scan, keyed on each directory's own marker. Reading the
    // project from the marker rather than from `projects_on_disk` matters:
    // a workweave whose `projects/<project>/` slot is missing is still a
    // workweave, and treating its live ref as an orphan is the direction
    // that turns a real branch into a "leftover".
    for (name, dir) in crate::workweave::list_workweave_dirs(ws_root) {
        if let Ok(Some(marker)) = crate::workspace::WorkweaveMarker::read(&dir) {
            out.push(EphemeralRefName::mint(
                &marker.project,
                &crate::manifest::WorkweaveName::new(&name),
            ));
        }
    }
    // The indexes, which are the only record of a `--dir` placement outside
    // every container (Q10). Consulted only for entries whose directory
    // actually exists, so a stale entry cannot resurrect a deleted workweave.
    for project in crate::workweave_index::projects_on_disk(ws_root) {
        if let Ok(Some(index)) = crate::workweave_index::read(ws_root, &project) {
            for (name, path) in &index.workweaves {
                if path.is_dir() {
                    out.push(EphemeralRefName::mint(
                        &project,
                        &crate::manifest::WorkweaveName::new(name),
                    ));
                }
            }
        }
    }
    out
}

/// Scan every project's receipt registry for receipts whose ref is not in
/// the store they name — `branch-model.md` §4.2's benign crash residue.
///
/// Only stores that are present and readable are considered. A receipt whose
/// store has gone is R4/Q14 territory (whether receipts are reclaimed in bulk
/// under a store-destroy is open), and retracting one here would answer that
/// by implementation.
///
/// `active_project` scopes the walk the way every other doctor scan is
/// scoped: `Some(name)` visits only that project's registry, `None` (the
/// `--all` path) visits every one. A receipt lives in exactly one project's
/// registry, so the scoping is a filter on which registries are opened
/// rather than a filter on findings.
fn scan_dangling_receipts(
    vcs: &dyn crate::vcs::Vcs,
    ws_root: &Path,
    active_project: Option<&str>,
    out: &mut Vec<CheckViolation>,
) {
    use crate::workweave_index::RefRegistry;

    for project in crate::workweave_index::projects_on_disk(ws_root) {
        if let Some(active) = active_project {
            if project.as_str() != active {
                continue;
            }
        }
        let registry = RefRegistry::for_project(ws_root, &project);
        let Ok(owned_refs) = registry.list_all() else {
            continue;
        };
        for owned in owned_refs {
            if !vcs.is_repo(owned.store()) {
                continue;
            }
            if matches!(
                vcs.resolve_local_branch_tip(owned.store(), owned.name()),
                Ok(None)
            ) {
                out.push(CheckViolation::DanglingRefReceipt {
                    project: project.clone(),
                    store_path: owned.store().to_path_buf(),
                    ref_name: owned.to_string(),
                });
            }
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

/// Scan branch-discipline (workweave-branch + the `branch-model.md` §7.2
/// canonical-store arms + stale-ephemeral-branches) across the workspace
/// rooted at `ws_root` (which must be the primary).
///
/// One symbolic-ref read per workweave checkout plus one branch listing
/// per canonical store. The check is VCS-neutral: it consumes only the
/// [`Vcs`] trait surface and never spells git plumbing.
///
/// `projects` supplies the tracking declarations §7.2's Detached arm
/// projects a reattach target from; pass every loaded project. With an empty
/// slice the arm still reports, it just cannot name a counterpart.
///
/// See:
///   * `branch-model.md` §7.2 (the canonical-store pass) and §5.1 (why
///     `projects/<project>/` is in scope).
///   * `docs/explanation/joints/clone-topology.md` (I3 — branch ownership).
///   * `docs/explanation/joints/shared-refs-drift.md` (safe/live doctrine,
///     applied here to refs instead of blobs).
///
/// [`Vcs`]: crate::vcs::Vcs
pub fn scan_branch_discipline(
    ws_root: &Path,
    vcs: &dyn crate::vcs::Vcs,
    projects: &[Project],
) -> Vec<CheckViolation> {
    let mut violations = Vec::new();
    let recorded = RecordedRefs::new(ws_root);

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
            ws_root,
            &recorded,
            &workweave_dir,
            marker.project.as_str(),
            &workweave_name,
            &mut violations,
        );
    }

    // (b) + (c) — the §7.2 pass over every canonical store under the
    // primary, `projects/<project>/` included.
    scan_canonical_stores(vcs, ws_root, projects, &recorded, &mut violations);

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
/// - **Canonical clone** (sub-kinds b/c: `StaleEphemeralBranchSafe`,
///   `StaleEphemeralBranchLive`, `StaleEphemeralBranchUnowned`): `repo_path`
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

    // Try every recorded container: a workweave repo path may live under
    // the compiled-in default, an env-var container, or any per-project
    // recorded container.
    for ww_parent in workweave_containers_for_scan(ws_root) {
        if let Ok(rel_from_ww_parent) = repo_path.strip_prefix(&ww_parent) {
            // (a) path: under <container>/<project>--<ww_name>/...
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
            return false;
        }
    }

    if let Ok(rel_from_ws) = repo_path.strip_prefix(ws_root) {
        // (b)/(c) path: under ws_root.
        // Convert to forward-slash string and look up in known_repos.
        let rel_str = rel_from_ws
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        // The project repo (§5.1) is not a manifest member, so it is not in
        // `known_repos`: `projects/<name>` is in scope exactly when `<name>`
        // is the active project. Without this arm every project-repo finding
        // would be filtered out of the default (project-scoped) run, which
        // is the scope hole §5.1 closes.
        if let Some(name) = rel_str.strip_prefix("projects/") {
            return name == active_project;
        }
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
/// branches in canonical stores.
///
/// **Recorded refs only.** The ref to destroy is re-resolved through the
/// receipt registry, and the destroy runs through
/// [`Vcs::delete_owned_ref`](crate::vcs::Vcs::delete_owned_ref), which takes
/// an [`OwnedRef`](crate::vcs::OwnedRef) plus a
/// [`DeletionWarrant`](crate::vcs::DeletionWarrant) and has no overload that
/// takes a name. That is the whole behavioural change: the shipped path
/// deleted whatever the scan had classified as safe, and the scan classified
/// by name shape, so a hand-made `<a>--<b>/<c>` branch was deleted for
/// looking like one of rwv's. Under R2 it is not rwv's and it survives.
///
/// Idempotent and information-preserving. The scan is re-run so each delete
/// sees the latest disk state, and the `Merged` warrant is established again
/// immediately before the destroy — the classification the report showed is
/// not carried over as authorization. Live-class and unowned branches are
/// never touched.
///
/// The receipt is retracted after a successful delete: leaving it would make
/// the registry claim a ref that no longer exists, which is the dangling
/// state [`scan_dangling_receipts`] exists to clear. Retracting the receipt
/// of a ref this call just destroyed is bookkeeping, not reclamation policy
/// — Q14 (whether receipts are reclaimed in bulk) stays open.
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
    projects: &[Project],
    active_project: Option<&str>,
    known_repos: &BTreeSet<RepoPath>,
) -> (Vec<(PathBuf, String)>, Vec<String>) {
    use crate::vcs::{DeletionWarrant, RawRefName};
    use crate::workweave_index::RefRegistry;

    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    let recorded = RecordedRefs::new(ws_root);

    // Re-scan so each delete sees the latest disk state and re-verifies
    // the safe-class precondition. `--fix` is meant to be idempotent: a
    // second invocation finds no safe-class violations to act on.
    for violation in scan_branch_discipline(ws_root, vcs, projects) {
        // Project-scope filter: only act on findings that belong to the
        // active project (or all when active_project is None).
        if let Some(ap) = active_project {
            if !branch_discipline_in_scope(&violation, ws_root, ap, known_repos) {
                continue;
            }
        }
        let (store_path, branch_name) = match violation {
            CheckViolation::BranchDiscipline {
                repo_path,
                sub_kind: BranchDisciplineKind::StaleEphemeralBranchSafe { branch, project: _ },
            } => (repo_path, branch),
            // Every other variant — live-class, unowned, and the
            // report-only (a)/(b) findings — is left untouched.
            _ => continue,
        };

        // Re-resolve the receipt. The scan's classification is a report;
        // the authorization has to be re-derived here, against the registry,
        // at the moment of the destroy.
        let name = RawRefName::new(branch_name.clone());
        let Some(rec) = recorded
            .for_store(&store_path)
            .into_iter()
            .find(|r| r.owned.name() == &name)
        else {
            errors.push(format!(
                "refusing to delete stale ephemeral branch `{}` in {}: rwv holds no \
                 ownership receipt for it (branch-model.md R2 — a ref that looks like \
                 rwv's is not rwv's)",
                branch_name,
                store_path.display()
            ));
            continue;
        };

        let Some(baseline) = vcs.head_revision(&store_path).ok() else {
            errors.push(format!(
                "refusing to delete stale ephemeral branch `{}` in {}: the store's own \
                 tip is unreadable, so no merged warrant can be established",
                branch_name,
                store_path.display()
            ));
            continue;
        };
        let Some(warrant) = DeletionWarrant::merged(vcs, &rec.owned, &baseline) else {
            errors.push(format!(
                "refusing to delete stale ephemeral branch `{}` in {}: its tip is not an \
                 ancestor of the store's tip, so it carries commits nothing else names",
                branch_name,
                store_path.display()
            ));
            continue;
        };

        match vcs.delete_owned_ref(&rec.owned, warrant) {
            Ok(()) => {
                if let Err(e) =
                    RefRegistry::for_project(ws_root, &rec.project).retract(&store_path, &name)
                {
                    errors.push(format!(
                        "deleted stale ephemeral branch `{}` in {} but could not retract \
                         its ownership receipt: {e}",
                        branch_name,
                        store_path.display()
                    ));
                }
                deleted.push((store_path, branch_name));
            }
            Err(e) => errors.push(format!(
                "failed to delete safe-class stale ephemeral branch `{}` in {}: {}",
                branch_name,
                store_path.display(),
                e
            )),
        }
    }

    (deleted, errors)
}

/// Apply `branch-model.md` §7.1's migration pass — the flat-name cutover's
/// other half.
///
/// Runs per workweave, and per repo checkout within it (members **and** the
/// project repo, §7.1's enumeration rule). The arms, in the doc's order:
///
///   1. attached to a pre-flat ref of this workweave's own namespace —
///      record a receipt at its current tip, then rename it to the flat name.
///      Automatic; nothing is lost, because a rename preserves the tip.
///   2. the flat ref present with no receipt — adopt it at its observed tip.
///      Without this arm a repo the migration half-processed falls into
///      arm 4 on re-run and is disowned forever.
///   3. detached with a pre-flat ref of this workweave at a different tip —
///      only with `--adopt-detached-checkouts`: give the pre-flat ref's name
///      up, then mint the flat one **at HEAD**, warning when that strands
///      commits. Without the flag, report-only (both tips, from the scan).
///   4. attached to anything else — report, do not touch. Under R2 these are
///      not rwv's refs.
///   5. detached with no pre-flat ref — as arm 3, minus the ref to give up.
///   6. unborn — nothing to attach a receipt to. Report-only.
///
/// Arm 7 (the legacy index and marker fields) is applied by `run_check`
/// alongside the marker migration it mirrors, before this runs — a receipt
/// cannot be recorded into a legacy index at all, so the field migration is
/// this pass's precondition rather than one of its arms.
///
/// # The three pass rules that are not arms
///
/// **No in-flight operation state.** §7.1: an operator who upgrades while a
/// sync is stopped mid-rebase resolves or aborts it first, without being told
/// to migrate. A workweave with op state is skipped with a message naming
/// `rwv abort`; the rest of the weave still migrates.
///
/// **The flat name must be reachable.** §7.1 assumes at most one ref per
/// (workweave, store); git holds `refs/heads/p--w` and `refs/heads/p--w/x`
/// as a file and a directory of the same name, so where two or more refs
/// share a namespace no arm can produce the flat one. That pair is skipped
/// before any arm runs — a receipt written for a rename that then fails
/// claims a pre-flat name, which §7.2 resolves to no workweave on disk and
/// so reads as stale and deletable. Collapsing the namespace is an
/// operator's call about which ref is the workweave's, so the skip names
/// the blocking refs and stops.
///
/// **Receipt before ref, durably, per repo.** Every arm records through
/// [`RefRegistry`], which fsyncs the file and its directory before returning,
/// and only then writes the ref. A crash leaves a dangling receipt — benign,
/// retractable by a later pass — never an unreceipted ref, which R2 disowns
/// permanently.
///
/// Idempotent over its own partial output, which is arm 2's whole purpose:
/// `record_created` is a no-op on an existing key (and returns the *first*
/// receipt, so a re-run cannot re-stamp `created_at` over a tip the operator
/// has since moved), and every arm re-observes before it acts.
///
/// [`RefRegistry`]: crate::workweave_index::RefRegistry
pub fn fix_branch_model_migration(
    ws_root: &Path,
    vcs: &dyn crate::vcs::Vcs,
    active_project: Option<&str>,
    adopt_detached: Option<crate::cli::consent::AdoptDetachedConsent>,
) -> (Vec<String>, Vec<String>) {
    use crate::vcs::{EphemeralRefName, HeadAttachment};
    use crate::workweave_index::RefRegistry;

    let mut applied = Vec::new();
    let mut errors = Vec::new();
    let recorded = RecordedRefs::new(ws_root);

    for (workweave_name, workweave_dir) in crate::workweave::list_workweave_dirs(ws_root) {
        let Ok(Some(marker)) = crate::workspace::WorkweaveMarker::read(&workweave_dir) else {
            continue;
        };
        if let Some(active) = active_project {
            if marker.project.as_str() != active {
                continue;
            }
        }
        // Pass rule: the migration runs only where no operation is in
        // flight. Checked per workweave, against the workweave's own op
        // state, because that is the granularity `rwv abort` resolves at.
        if let Err(e) = crate::op_state::check_no_op_in_progress(&[workweave_dir.as_path()]) {
            errors.push(format!(
                "{}: skipped the branch-model migration — an operation is in flight \
                 ({e}). Resolve or `rwv abort` it, then re-run `rwv doctor --fix`",
                workweave_dir.display()
            ));
            continue;
        }

        let flat = EphemeralRefName::mint(
            &marker.project,
            &crate::manifest::WorkweaveName::new(&workweave_name),
        );
        let mut registry = RefRegistry::for_project(ws_root, &marker.project);

        for abs in workweave_checkouts(vcs, &workweave_dir, marker.project.as_str()) {
            let store = crate::workweave::receipt_store_for(&abs);
            let (flat_present, legacy_refs) = refs_in_workweave_namespace(vcs, &store, &flat);

            // Pass rule: the migration runs only where the flat name is
            // reachable. Two or more refs under one workweave's namespace put
            // it out of reach — git will not create `refs/heads/{flat}` as a
            // ref while `refs/heads/{flat}/` exists as a directory, so the
            // rename cannot succeed however the arms are ordered, and the
            // consented detached arms would delete one sibling and *then*
            // fail to mint the flat name.
            //
            // Checked before any arm because every arm records its receipt
            // BEFORE it writes the ref (§7.1's receipt-before-ref rule), so acting here
            // persists an ownership claim for a rename that did not happen —
            // and a receipt for a pre-flat name is worse than no receipt at
            // all: §7.2 derives the owning workweave by parsing the ref name,
            // and under flat naming a name with a segment resolves to no
            // workweave on disk. The branch is then judged stale, and being
            // receipted is exactly what lifts it out of Unowned into the
            // classes `--fix` deletes from.
            if legacy_refs.len() > 1 {
                let blocking = legacy_refs
                    .iter()
                    .map(|r| format!("`{r}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                errors.push(format!(
                    "{}: skipped the branch-model migration for `{flat}` — {} refs share \
                     that namespace ({blocking}), and git cannot create the ref `{flat}` \
                     while any ref exists under `{flat}/`. rwv recorded no ownership \
                     receipt for `{flat}` and renamed nothing here; the rest of the \
                     migration is unaffected. Which of those refs is this workweave's \
                     branch, and where the others belong, is not rwv's call to make — \
                     leave at most one ref under `{flat}/`, then re-run `rwv doctor --fix` \
                     to migrate it",
                    store.display(),
                    legacy_refs.len()
                ));
                continue;
            }

            let flat_raw = flat.to_raw();
            let flat_recorded = matches!(registry.lookup(&store, &flat_raw), Ok(Some(_)));

            match vcs.head_attachment(&abs) {
                // Arm 1: the common case, fully automatic.
                Ok(HeadAttachment::Attached(a)) if a.legacy_name_under(&flat).is_some() => {
                    let legacy = a
                        .legacy_name_under(&flat)
                        .expect("matched Some in the guard");
                    match migrate_legacy_ref(vcs, &mut registry, &store, &a, legacy, &flat) {
                        Ok(msg) => applied.push(msg),
                        Err(e) => errors.push(format!("{}: {e}", abs.display())),
                    }
                }
                // Arm 2: the flat ref is there but unowned. Reached from any
                // attachment state — the ref exists in the store whether or
                // not this checkout is standing on it.
                Ok(_) if flat_present && !flat_recorded => {
                    match adopt_flat_ref(vcs, &mut registry, &store, &flat) {
                        Ok(msg) => applied.push(msg),
                        Err(e) => errors.push(format!("{}: {e}", abs.display())),
                    }
                }
                // Arms 3 and 5, and only with the operator's consent.
                Ok(HeadAttachment::Detached(d)) => {
                    let Some(consent) = adopt_detached else {
                        continue;
                    };
                    match adopt_detached_workweave_checkout(
                        vcs,
                        &mut registry,
                        &store,
                        &d,
                        legacy_refs.first(),
                        &flat,
                        consent,
                    ) {
                        Ok(msgs) => applied.extend(msgs),
                        Err(e) => errors.push(format!("{}: {e}", abs.display())),
                    }
                }
                // Arms 4 and 6: report-only. The scan has already said so.
                Ok(_) => {}
                Err(_) => {}
            }
        }
    }

    let _ = &recorded;
    (applied, errors)
}

/// §7.1 arm 1: adopt a pre-flat ref into a receipt, then rename it flat.
///
/// §3.4 derives a rename as a DESTROY of the old name plus a birth of the
/// new, so the DESTROY takes the old name's receipt and a warrant, and the
/// birth's receipt is on disk — fsynced — before the ref write, because an
/// [`OwnedRef`] exists only after the registry has persisted one.
///
/// # The three crash windows, and what a re-run finds
///
/// **After the legacy receipt, before the flat one.** The ref still exists at
/// the recorded tip; the re-run renames it. Unless the operator committed on
/// it in between, which is the case the retraction below exists for.
///
/// **After both receipts, before the rename.** An extra receipt for a ref
/// that does not exist yet. `fix_dangling_receipts` runs earlier in the same
/// `--fix` and retracts it, and this arm then records it afresh at the tip it
/// observes now — which is the right tip, not the crashed run's.
///
/// **After the rename, before the retraction.** A receipt for a name that is
/// gone. Retracted below; if the process dies between the two,
/// `fix_dangling_receipts` clears it on the next run.
///
/// [`DeletionWarrant::unmoved`]: crate::vcs::DeletionWarrant::unmoved
/// [`OwnedRef`]: crate::vcs::OwnedRef
fn migrate_legacy_ref(
    vcs: &dyn crate::vcs::Vcs,
    registry: &mut crate::workweave_index::RefRegistry,
    store: &Path,
    attached: &crate::vcs::AttachedRef,
    legacy: crate::vcs::LegacyEphemeralRefName,
    flat: &crate::vcs::EphemeralRefName,
) -> anyhow::Result<String> {
    use crate::vcs::DeletionWarrant;

    // Re-observe before acting: the scan's classification is a report, and
    // this checkout may have been switched off the ref since. `git branch -m`
    // would succeed anyway (a rename needs no worktree), and would leave a
    // flat receipt for a ref this workweave is no longer standing on.
    vcs.verify_attachment(attached)?;

    let label = legacy.to_string();
    let raw = legacy.to_raw();
    let tip = vcs
        .resolve_local_branch_tip(store, &raw)?
        .ok_or_else(|| anyhow::anyhow!("branch `{label}` vanished before it could be migrated"))?;

    // A receipt for the pre-flat name can only have come from a previous,
    // crashed run of this arm — nothing else in the tree adopts an observed
    // name. If the operator committed on the branch in between, that receipt
    // records a tip the ref will never return to, and the `Unmoved` warrant
    // below would refuse it forever: the migration would be permanently
    // wedged on the one workweave someone is actually working in.
    //
    // So retract the stale one and adopt at the tip observed now. This is the
    // "caller that genuinely re-creates a ref retracts the old receipt, then
    // records anew" path `record_created`'s contract names, and it is safe
    // for the reason the whole arm is safe: the receipt authorizes a rename,
    // and a rename preserves the tip. It cannot authorize a loss.
    if let Some(stale) = registry.lookup(store, &raw)? {
        if stale.created_at() != &tip {
            registry.retract(store, &raw)?;
        }
    }
    let owned_legacy = registry.adopt_legacy(store, legacy, tip)?;
    let owned_flat =
        registry.record_created(store, flat.clone(), owned_legacy.created_at().clone())?;

    let warrant = DeletionWarrant::unmoved(vcs, &owned_legacy).ok_or_else(|| {
        anyhow::anyhow!(
            "branch `{label}` moved while it was being migrated (recorded at {}); \
             re-run `rwv doctor --fix`",
            owned_legacy.created_at().display_str()
        )
    })?;
    vcs.rename_owned_ref(&owned_legacy, &owned_flat, warrant)?;

    // Retracted AFTER the ref is gone, never before: the reverse order would
    // leave an unreceipted ref, which R2 disowns permanently.
    if let Err(e) = registry.retract(store, &raw) {
        return Ok(format!(
            "migrated `{label}` → `{flat}` in {} (its old ownership receipt could not be \
             retracted: {e}; `rwv doctor --fix` will clear it)",
            store.display()
        ));
    }
    Ok(format!(
        "migrated `{label}` → `{flat}` in {}",
        store.display()
    ))
}

/// §7.1 arm 2: record a receipt for a flat ref that exists without one.
///
/// The tip is read here and recorded as `created_at`, which is what the doc
/// specifies ("adopt it: write a receipt at the observed tip") — and what
/// makes the pass idempotent over its own partial output, because a re-run
/// finds the receipt already there and `record_created` does nothing.
fn adopt_flat_ref(
    vcs: &dyn crate::vcs::Vcs,
    registry: &mut crate::workweave_index::RefRegistry,
    store: &Path,
    flat: &crate::vcs::EphemeralRefName,
) -> anyhow::Result<String> {
    let tip = vcs
        .resolve_local_branch_tip(store, &flat.to_raw())?
        .ok_or_else(|| anyhow::anyhow!("branch `{flat}` vanished before it could be adopted"))?;
    registry.record_created(store, flat.clone(), tip.clone())?;
    Ok(format!(
        "adopted `{flat}` in {} at {} (rwv now holds an ownership receipt for it)",
        store.display(),
        tip.display_str()
    ))
}

/// §7.1 arms 3 and 5: mint the workweave's flat ref at a detached HEAD.
///
/// Arm 5 is arm 3 without the pre-flat ref. When there is one, git cannot
/// hold both `refs/heads/p--w` and `refs/heads/p--w/<segment>`, so the flat
/// name can only exist once the pre-flat one stops — which is precisely the
/// stranding arm 3 makes the caller warn about, and why the operator's
/// consent is required even when nothing is lost.
///
/// The warrant for that DESTROY is [`Merged`] when the pre-flat tip is an
/// ancestor of HEAD (nothing is stranded, and rwv can prove it) and
/// [`adopt_detached`] otherwise — the flag *is* the consent to the loss.
/// Structural either way: ancestry, never wall-clock.
///
/// [`Merged`]: crate::vcs::DeletionWarrant::merged
/// [`adopt_detached`]: crate::vcs::DeletionWarrant::adopt_detached
fn adopt_detached_workweave_checkout(
    vcs: &dyn crate::vcs::Vcs,
    registry: &mut crate::workweave_index::RefRegistry,
    store: &Path,
    detached: &crate::vcs::DetachedHead,
    legacy: Option<&crate::vcs::LegacyEphemeralRefName>,
    flat: &crate::vcs::EphemeralRefName,
    consent: crate::cli::consent::AdoptDetachedConsent,
) -> anyhow::Result<Vec<String>> {
    use crate::vcs::DeletionWarrant;

    let mut msgs = Vec::new();
    let head = detached.at().clone();

    if let Some(legacy) = legacy {
        let label = legacy.to_string();
        let raw = legacy.to_raw();
        let tip = vcs.resolve_local_branch_tip(store, &raw)?.ok_or_else(|| {
            anyhow::anyhow!("branch `{label}` vanished before it could be given up")
        })?;
        let owned = registry.adopt_legacy(store, legacy.clone(), tip.clone())?;
        let (warrant, note) = match DeletionWarrant::merged(vcs, &owned, &head) {
            Some(w) => (w, String::new()),
            None => (
                DeletionWarrant::adopt_detached(consent),
                format!(
                    " — WARNING: this STRANDED the commits at {}, which HEAD does not carry; \
                     they are reachable only through {}'s reflog",
                    tip.display_str(),
                    store.display()
                ),
            ),
        };
        vcs.delete_owned_ref(&owned, warrant)?;
        // After the ref is gone, never before.
        let _ = registry.retract(store, &raw);
        msgs.push(format!(
            "gave up `{label}` in {} to make room for `{flat}`{note}",
            store.display()
        ));
    }

    let owned_flat = registry.record_created(store, flat.clone(), head.clone())?;
    vcs.adopt_detached_checkout(detached, &owned_flat, consent)?;
    msgs.push(format!(
        "adopted the detached checkout at {} onto `{flat}`",
        detached.repo().display()
    ));
    Ok(msgs)
}

/// Apply the `rwv doctor --fix --reattach-checkouts` reattach for
/// `branch-model.md` §7.2's Detached arm.
///
/// Reattaches a detached canonical store to its tracking declaration's local
/// counterpart **only** when that counterpart exists and its tip equals
/// HEAD. Both halves are re-observed here, not taken from the scan: the
/// counterpart is re-projected, the tip re-resolved, and
/// [`Vcs::reattach_head`](crate::vcs::Vcs::reattach_head) itself refuses if
/// the repo's HEAD state moved since.
///
/// **Honest but partial, by design.** That condition is false for the
/// ordinary post-fetch state — a stale local counterpart with HEAD at the
/// lock SHA — which is most detached repos in most weaves (§6 item 2). This
/// reattaches the minority it can prove safe. It is not weave-wide
/// reattachment and must not be described as one.
///
/// `consent` is the [`ReattachConsent`] the CLI minted from
/// `--reattach-checkouts`; without the flag this is not called at all, and
/// `--fix` reports the correct `git switch` instead.
///
/// [`ReattachConsent`]: crate::cli::consent::ReattachConsent
pub fn fix_detached_canonicals(
    ws_root: &Path,
    vcs: &dyn crate::vcs::Vcs,
    projects: &[Project],
    active_project: Option<&str>,
    known_repos: &BTreeSet<RepoPath>,
    consent: crate::cli::consent::ReattachConsent,
) -> (Vec<(PathBuf, String)>, Vec<String>) {
    use crate::vcs::RawRefName;

    let mut reattached = Vec::new();
    let mut errors = Vec::new();

    let stores = canonical_stores(vcs, ws_root, projects);
    for store in &stores {
        if let Some(ap) = active_project {
            let probe = CheckViolation::BranchDiscipline {
                repo_path: store.path.clone(),
                sub_kind: BranchDisciplineKind::Detached {
                    expected_ref: String::new(),
                    recorded_ref: None,
                    at_sha: String::new(),
                    legacy_branch: None,
                },
            };
            if !branch_discipline_in_scope(&probe, ws_root, ap, known_repos) {
                continue;
            }
        }

        // Re-observe. Anything but Detached means the state the fix was
        // planned against no longer holds.
        let Ok(observed @ crate::vcs::HeadAttachment::Detached(_)) =
            vcs.head_attachment(&store.path)
        else {
            continue;
        };
        let crate::vcs::HeadAttachment::Detached(detached) = &observed else {
            unreachable!("matched Detached above")
        };
        let Some(counterpart) = store.local_counterpart(vcs) else {
            continue;
        };
        // §7.2's condition. Not "the counterpart exists" alone: reattaching
        // to a counterpart whose tip differs from HEAD would move the
        // operator's working state onto a different commit, which is a
        // MOVE wearing an ATTACH's clothes.
        let tip_matches = matches!(
            vcs.resolve_local_branch_tip(&store.path, &RawRefName::new(counterpart.as_str())),
            Ok(Some(ref tip)) if tip == detached.at()
        );
        if !tip_matches {
            continue;
        }

        let label = counterpart.to_string();
        match vcs.reattach_head(observed, &counterpart, consent) {
            Ok(()) => reattached.push((store.path.clone(), label)),
            Err(e) => errors.push(format!(
                "failed to reattach detached canonical {} to `{}`: {}",
                store.path.display(),
                label,
                e
            )),
        }
    }

    (reattached, errors)
}

/// Apply the `rwv doctor --fix` retraction for dangling ownership receipts
/// (`branch-model.md` §4.2).
///
/// Safe by construction: a receipt naming a ref that does not exist
/// authorizes nothing — no warrant can be built against an absent ref — so
/// dropping it destroys no capability and no work.
///
/// The absence check lives in [`scan_dangling_receipts`] and nowhere else,
/// which is why this runs it here rather than reusing an earlier scan's
/// output. A second copy of the check in this function would be a safety
/// property no test can reach: the scan runs microseconds earlier in the
/// same call, so no fixture can open a window between them, and an
/// unreachable guard is one that silently stops holding.
pub fn fix_dangling_receipts(
    ws_root: &Path,
    vcs: &dyn crate::vcs::Vcs,
    active_project: Option<&str>,
) -> (Vec<(PathBuf, String)>, Vec<String>) {
    use crate::workweave_index::RefRegistry;

    let mut retracted = Vec::new();
    let mut errors = Vec::new();

    let mut violations = Vec::new();
    scan_dangling_receipts(vcs, ws_root, active_project, &mut violations);
    for violation in violations {
        let CheckViolation::DanglingRefReceipt {
            project,
            store_path,
            ref_name,
        } = violation
        else {
            continue;
        };
        let name = crate::vcs::RawRefName::new(ref_name.clone());
        match RefRegistry::for_project(ws_root, &project).retract(&store_path, &name) {
            Ok(true) => retracted.push((store_path, ref_name)),
            Ok(false) => {}
            Err(e) => errors.push(format!(
                "failed to retract dangling ownership receipt for `{}` in {}: {e}",
                ref_name,
                store_path.display()
            )),
        }
    }

    (retracted, errors)
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

            // Coverage: every manifest repo should have a lock entry.
            // Distinct from the freshness comparison above — a repo absent
            // from the lock is invisible to it (`lock.iter_entries()` never
            // produces it).
            for repo_path in project.manifest.iter_repo_paths() {
                if !lock.contains_repo(repo_path) {
                    violations.push(CheckViolation::IncompleteLock {
                        project: project.name.clone(),
                        repo: repo_path.clone(),
                    });
                }
            }
        }
    }

    violations
}

/// The `git switch` a branch-discipline finding should advise, spelled for
/// what is actually on disk.
///
/// `git switch <name>` when rwv holds a receipt for the workweave's ref —
/// the branch exists, and returning to it is the repair. `git switch -c`
/// only when there is none, because that is the only case where a branch has
/// to be created.
///
/// Getting this backwards is not a typo. Asked to switch to a name it cannot
/// find as a local branch, git does not refuse: `checkout.guess` invents the
/// branch from a remote-tracking ref of the same name, a tag-shaped name
/// detaches HEAD, and a path-shaped name is read as a pathspec and reverts
/// the operator's uncommitted edits to it — all exiting 0. Advice that says
/// `-c` for an existing branch fails outright (`already exists`), which is
/// merely useless; advice that omits `-c` for an absent one silently does
/// one of those three things.
fn reattach_advice(recorded_ref: Option<&str>, expected_prefix: &str) -> String {
    match recorded_ref {
        Some(name) => format!(
            "rwv holds a receipt for `{name}` in this repo's canonical store — \
             use `git switch {name}` to return to it"
        ),
        None => format!(
            "rwv holds no receipt for an ephemeral ref in this repo's canonical \
             store, so there is none to return to — `git switch -c \
             {expected_prefix}/main` creates one, or recreate the workweave so \
             rwv records it"
        ),
    }
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
                CheckViolation::IncompleteLock { project, repo } => (
                    crate::integration::Severity::Error,
                    format!(
                        "incomplete lock in {project}: {repo} has no rwv.lock entry; \
                         run `rwv lock` to add it"
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
                            "; run `rwv workweave <project> create --replace-existing` to \
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
                CheckViolation::LegacyWorkweaveIndex { index_path, .. } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{} is a legacy workweave index written before ref-ownership \
                         receipts existed (no `receipts` field), so rwv can neither \
                         record nor destroy an ephemeral ref for this project; run \
                         `rwv doctor --fix` to migrate it (branch-model.md §7.1 arm 7)",
                        index_path.display()
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
                        WorkweaveTreeIntegrityKind::StaleRegistryEntry {
                            project,
                            workweave_name,
                            recorded_path,
                            reason,
                        } => format!(
                            "workweave-index: stale entry `{}` in project `{}` \
                             (recorded at `{}`): {reason}; run `rwv doctor --fix` \
                             to prune",
                            workweave_name,
                            project,
                            recorded_path.display()
                        ),
                        WorkweaveTreeIntegrityKind::UnregisteredWorkweave {
                            project,
                            workweave_name,
                        } => format!(
                            "{}: workweave `{}` for project `{}` is present on \
                             disk but not recorded in `.rwv-workweave-index`; \
                             run `rwv doctor --fix` to adopt it",
                            workweave_dir.display(),
                            workweave_name,
                            project
                        ),
                        WorkweaveTreeIntegrityKind::TrackedIndex {
                            project,
                            index_path,
                        } => format!(
                            "{}: `.rwv-workweave-index` for project `{}` is \
                             tracked by the project repo; the index is \
                             machine-local — run `git rm --cached {}` and add \
                             `.rwv-workweave-index` to `.gitignore`",
                            index_path.display(),
                            project,
                            index_path.display()
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
                            expected_ref,
                            recorded_ref,
                        } => format!(
                            "{}: workweave checkout is on shared-branch `{}` (expected its \
                             ephemeral branch `{}`); manual `git switch` inside a \
                             workweave breaks the I3 branch-ownership invariant — \
                             {} (report-only; no rwv --fix path)",
                            repo_path.display(),
                            actual_branch,
                            expected_ref,
                            reattach_advice(recorded_ref.as_deref(), expected_ref)
                        ),
                        BranchDisciplineKind::ForeignEphemeral {
                            actual_branch,
                            expected_ref,
                            recorded_ref,
                        } => format!(
                            "{}: workweave checkout is on `{}`, a ref rwv recorded for a \
                             different workweave (expected its ephemeral branch `{}`); \
                             {} (report-only; no rwv --fix path)",
                            repo_path.display(),
                            actual_branch,
                            expected_ref,
                            reattach_advice(recorded_ref.as_deref(), expected_ref)
                        ),
                        BranchDisciplineKind::Detached {
                            expected_ref,
                            recorded_ref,
                            at_sha,
                            legacy_branch,
                        } => match legacy_branch {
                            // §7.1 arm 3: both tips, side by side, and the
                            // two remediations in the order the doc puts
                            // them — reattach first, adopt second.
                            Some(legacy) => {
                                safe_to_fix = false;
                                let relation = if legacy.strands_commits {
                                    "a commit HEAD does not carry"
                                } else {
                                    "an ancestor of HEAD"
                                };
                                let stranding = if legacy.strands_commits {
                                    format!(
                                        ". THAT STRANDS the commits on `{}`: adopting gives \
                                         its name up, and they stay reachable only through \
                                         the reflog",
                                        legacy.branch
                                    )
                                } else {
                                    String::new()
                                };
                                format!(
                                    "{}: workweave checkout is detached at {} while its \
                                     pre-flat branch `{}` sits at {} ({}). Reattach to it \
                                     (`git switch {}`) and re-run `rwv doctor --fix`, which \
                                     renames it to `{}`; or run `rwv doctor --fix \
                                     --adopt-detached-checkouts` to mint `{}` here at {} \
                                     instead{}",
                                    repo_path.display(),
                                    at_sha,
                                    legacy.branch,
                                    legacy.tip_sha,
                                    relation,
                                    legacy.branch,
                                    expected_ref,
                                    expected_ref,
                                    at_sha,
                                    stranding,
                                )
                            }
                            // §7.1 arm 5.
                            None => format!(
                                "{}: workweave checkout is in detached-HEAD state at {} \
                                 (expected its ephemeral branch `{}`); {} — or run \
                                 `rwv doctor --fix --adopt-detached-checkouts` to mint \
                                 `{}` here at {}",
                                repo_path.display(),
                                at_sha,
                                expected_ref,
                                reattach_advice(recorded_ref.as_deref(), expected_ref),
                                expected_ref,
                                at_sha,
                            ),
                        },
                        // §7.1 arm 1 — the fully automatic migration case.
                        BranchDisciplineKind::UnmigratedEphemeralBranch {
                            actual_branch,
                            expected_ref,
                        } => format!(
                            "{}: workweave checkout is on `{}`, the pre-flat \
                             `<project>--<workweave>/<segment>` shape rwv no longer mints \
                             (branch-model.md §3.5); `rwv doctor --fix` records an \
                             ownership receipt for it and renames it to `{}` — a rename \
                             preserves the tip, so no commit moves",
                            repo_path.display(),
                            actual_branch,
                            expected_ref,
                        ),
                        // §7.1 arm 2.
                        BranchDisciplineKind::UnrecordedEphemeralBranch { branch } => format!(
                            "{}: branch `{}` is this workweave's ephemeral ref but rwv \
                             holds no ownership receipt for it, so under branch-model.md \
                             R2 it is not rwv's to delete and `rwv workweave delete` will \
                             leave it behind; `rwv doctor --fix` adopts it at its current \
                             tip",
                            repo_path.display(),
                            branch,
                        ),
                        // §7.1 arm 6.
                        BranchDisciplineKind::UnbornCheckout { branch } => {
                            safe_to_fix = false;
                            format!(
                                "{}: workweave checkout is on branch `{}`, which has no \
                                 commits yet — there is no revision to record an ownership \
                                 receipt against, so the branch-model migration has nothing \
                                 to adopt here. Make an initial commit, then re-run \
                                 `rwv doctor --fix`",
                                repo_path.display(),
                                branch,
                            )
                        }
                        BranchDisciplineKind::CanonicalHoldsLiveWorkweaveRef {
                            actual_branch,
                            workweave_name,
                        } => {
                            safe_to_fix = false;
                            format!(
                                "{}: canonical store is checked out on `{}`, a ref rwv \
                                 recorded for workweave `{}` — which is still on disk. git \
                                 forbids one branch being checked out twice in the same \
                                 store, so this directory was moved or copied \
                                 (report-only; no rwv --fix path — nothing here can tell \
                                 which of the two checkouts is the real one)",
                                repo_path.display(),
                                actual_branch,
                                workweave_name
                            )
                        }
                        BranchDisciplineKind::CanonicalHoldsLeakedRef {
                            actual_branch,
                            project,
                        } => {
                            safe_to_fix = false;
                            format!(
                                "{}: canonical store is checked out on `{}`, a ref rwv \
                                 recorded for project `{}` whose workweave is gone — a \
                                 leak. `--fix` cannot reclaim it while this store's own \
                                 HEAD is on it (a branch a worktree uses cannot be \
                                 deleted); `git switch <tracking-branch>` first, then \
                                 re-run `rwv doctor --fix`",
                                repo_path.display(),
                                actual_branch,
                                project
                            )
                        }
                        BranchDisciplineKind::CanonicalDetached {
                            at_sha,
                            counterpart,
                            reattachable,
                        } => match (counterpart, reattachable) {
                            (Some(name), true) => format!(
                                "{}: canonical store is in detached-HEAD state at {}; its \
                                 tracking counterpart `{}` exists and points at the same \
                                 commit — `rwv doctor --fix --reattach-checkouts` will \
                                 reattach it, or `git switch {}` by hand",
                                repo_path.display(),
                                at_sha,
                                name,
                                name
                            ),
                            (Some(name), false) => format!(
                                "{}: canonical store is in detached-HEAD state at {}; its \
                                 tracking counterpart `{}` does not exist or points \
                                 elsewhere, so reattaching would move your working state \
                                 onto a different commit — reconcile `{}` with {} \
                                 yourself, then `git switch {}` \
                                 (report-only; --reattach-checkouts will not fire here)",
                                repo_path.display(),
                                at_sha,
                                name,
                                name,
                                at_sha,
                                name
                            ),
                            (None, _) => format!(
                                "{}: canonical store is in detached-HEAD state at {}; no \
                                 tracking declaration resolves for this repo, so rwv \
                                 cannot name a branch to reattach to \
                                 (report-only; `git switch <branch>` by hand)",
                                repo_path.display(),
                                at_sha
                            ),
                        },
                        BranchDisciplineKind::StaleEphemeralBranchSafe { branch, project } => {
                            format!(
                                "{}: leaked ephemeral branch `{}`, recorded by project `{}` \
                                 for a workweave that is gone (safe class — rwv holds an \
                                 ownership receipt for it and its tip is reachable from the \
                                 store's tip; `rwv doctor --fix` will delete it)",
                                repo_path.display(),
                                branch,
                                project
                            )
                        }
                        BranchDisciplineKind::StaleEphemeralBranchUnowned { branch } => {
                            safe_to_fix = false;
                            format!(
                                "{}: branch `{}` carries the pre-flat \
                                 `<project>--<workweave>/<segment>` shape and no workweave \
                                 on disk claims that namespace, but rwv holds no ownership \
                                 receipt for it — under branch-model.md R2 it is not rwv's \
                                 to delete, and §7.3 forbids guessing whose workweave it \
                                 was. `--fix` will never touch it; remove it by hand if it \
                                 is yours to remove",
                                repo_path.display(),
                                branch
                            )
                        }
                        BranchDisciplineKind::StaleEphemeralBranchLive {
                            branch,
                            project,
                            tip_sha,
                        } => {
                            // Live-class: tip carries commits not reachable
                            // from the primary; `doctor --fix` must not touch
                            // it. Mark the issue accordingly so the integration
                            // runner's user-held-issues partition leaves it
                            // alone.
                            safe_to_fix = false;
                            format!(
                                "{}: leaked ephemeral branch `{}`, recorded by project `{}` \
                                 for a workweave that is gone, carries unique commits at \
                                 tip `{}` (live class — `--fix` will not touch this; \
                                 recover or delete by hand)",
                                repo_path.display(),
                                branch,
                                project,
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
                CheckViolation::DanglingRefReceipt {
                    project,
                    store_path,
                    ref_name,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{}: project `{}` holds an ownership receipt for `{}` but no such \
                         ref is there — the benign residue of a crash between the receipt \
                         write and the ref creation (branch-model.md §4.2). It authorizes \
                         nothing; run `rwv doctor --fix` to retract it",
                        store_path.display(),
                        project,
                        ref_name
                    ),
                ),
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
///
/// `reattach` is the [`ReattachConsent`] the CLI minted from
/// `--reattach-checkouts`, or `None` when the operator did not pass it.
/// It gates exactly one thing: whether `--fix` *reattaches* a detached
/// canonical store (`branch-model.md` §7.2's Detached arm) or only reports
/// it with the `git switch` that would. Changing what a checkout's commits
/// hang off is an ATTACH, and an ATTACH that is not a birth needs the
/// operator's consent — which is why the token is threaded down here rather
/// than a bool.
///
/// [`ReattachConsent`]: crate::cli::consent::ReattachConsent
pub fn run_check(
    ctx: &crate::workspace::WorkspaceContext,
    fix: bool,
    scope_all: bool,
    reattach: Option<crate::cli::consent::ReattachConsent>,
    adopt_detached: Option<crate::cli::consent::AdoptDetachedConsent>,
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
    // dirs, foreign-primary markers, plus the registry reconciliation
    // findings (`stale-registry-entry`, `unregistered-workweave`,
    // `tracked-index`). Chain-anomaly / unregistered-dir / foreign-primary /
    // tracked-index are report-only. `dangling-parent`, `stale-registry-entry`,
    // and `unregistered-workweave` gain a `--fix` path. Runs from the primary
    // weave so the scan covers all workweaves belonging to this workspace.
    let mut dangling_parent_fix_errors: Vec<String> = Vec::new();
    let mut registry_fix_errors: Vec<String> = Vec::new();
    for v in scan_workweave_tree_integrity(ctx.primary_path()) {
        if fix {
            match &v {
                CheckViolation::WorkweaveTreeIntegrity {
                    workweave_dir,
                    sub_kind: WorkweaveTreeIntegrityKind::DanglingParent { .. },
                } => {
                    match fix_dangling_parent(workweave_dir, ctx.primary_path()) {
                        Ok(true) => {
                            println!(
                                "[fixed] core: re-pointed dangling parent of {} to primary",
                                workweave_dir.display()
                            );
                            continue;
                        }
                        Ok(false) => continue, // race
                        Err(e) => {
                            dangling_parent_fix_errors.push(e.to_string());
                            // Fall through and still report.
                        }
                    }
                }
                CheckViolation::WorkweaveTreeIntegrity {
                    sub_kind:
                        WorkweaveTreeIntegrityKind::StaleRegistryEntry {
                            project,
                            workweave_name,
                            recorded_path,
                            ..
                        },
                    ..
                } => {
                    let project_name = crate::manifest::ProjectName::new(project.clone());
                    match fix_stale_registry_entry(
                        ctx.primary_path(),
                        &project_name,
                        workweave_name,
                    ) {
                        Ok(()) => {
                            println!(
                                "[fixed] core: pruned stale registry entry `{}` \
                                 → {} in project `{}`",
                                workweave_name,
                                recorded_path.display(),
                                project
                            );
                            continue;
                        }
                        Err(e) => {
                            registry_fix_errors.push(format!(
                                "prune of stale entry `{}` in `{}` failed: {e}",
                                workweave_name, project
                            ));
                        }
                    }
                }
                CheckViolation::WorkweaveTreeIntegrity {
                    workweave_dir,
                    sub_kind:
                        WorkweaveTreeIntegrityKind::UnregisteredWorkweave {
                            project,
                            workweave_name,
                        },
                } => {
                    let project_name = crate::manifest::ProjectName::new(project.clone());
                    match fix_unregistered_workweave(
                        ctx.primary_path(),
                        &project_name,
                        workweave_name,
                        workweave_dir,
                    ) {
                        Ok(()) => {
                            println!(
                                "[fixed] core: adopted workweave `{}` at {} into \
                                 project `{}`'s registry",
                                workweave_name,
                                workweave_dir.display(),
                                project
                            );
                            continue;
                        }
                        Err(e) => {
                            registry_fix_errors.push(format!(
                                "adopt of workweave `{}` at {} failed: {e}",
                                workweave_name,
                                workweave_dir.display()
                            ));
                        }
                    }
                }
                _ => {}
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

    // Dangling ownership receipts (branch-model.md §4.2): a receipt whose
    // ref never appeared. `--fix` retracts them; the retraction runs before
    // the branch-discipline scan below so the scan sees the cleaned
    // registry.
    let receipt_scope = if scope_all {
        None
    } else {
        active_project_name.as_ref().map(|n| n.as_str())
    };
    if fix {
        let (retracted, retract_errs) =
            fix_dangling_receipts(ctx.primary_path(), &git, receipt_scope);
        for (store_path, ref_name) in &retracted {
            println!(
                "[fixed] core: retracted dangling ownership receipt for `{}` in {}",
                ref_name,
                store_path.display()
            );
        }
        for msg in retract_errs {
            all_issues_branch_discipline_errors.push(msg);
        }
    } else {
        scan_dangling_receipts(&git, ctx.primary_path(), receipt_scope, &mut violations);
    }

    // Branch-discipline: (a) workweave-branch, (b) the §7.2 canonical-store
    // arms, (c) stale-ephemeral-branches. (a) and (b) are report-only except
    // for the Detached arm, which `--fix --reattach-checkouts` repairs; (c)
    // splits into safe-class (deletable under --fix, receipt + warrant),
    // live-class and unowned (never auto-deleted). The --fix paths are
    // applied below before violations are emitted so a successful repair is
    // reported as `[fixed]` instead of surfacing the paired warning.
    //
    // Scope: when scope_all is false and an active project is set, filter
    // findings to only those belonging to the active project. This mirrors
    // the legacy_role_primary filter above and prevents the doctor scoped
    // to a single active project from touching another project's stale
    // ephemeral branches.
    //
    // `branch-model.md` §7.1 arm 7 — the legacy index field, alongside the
    // legacy-marker `parent:` migration above that it mirrors. It runs
    // *before* the migration pass because `RefRegistry::record_created`
    // refuses against an index with no `receipts` field: adding the field is
    // the pass's precondition, not one of its arms. Adding it is not itself
    // an ownership claim — the registry it produces is empty, and every
    // pre-existing ref stays unowned until an arm records it explicitly.
    if fix {
        for project in crate::workweave_index::projects_on_disk(ctx.primary_path()) {
            if let Some(active) = receipt_scope {
                if project.as_str() != active {
                    continue;
                }
            }
            let mut registry =
                crate::workweave_index::RefRegistry::for_project(ctx.primary_path(), &project);
            match registry.migrate_legacy_index() {
                Ok(true) => println!(
                    "[fixed] core: added the ref-ownership registry to {}",
                    crate::workweave_index::index_path(ctx.primary_path(), &project).display()
                ),
                Ok(false) => {}
                Err(e) => all_issues_branch_discipline_errors.push(format!(
                    "failed to migrate {}'s workweave index to the ref-ownership \
                     registry: {e}",
                    project
                )),
            }
        }
    } else {
        for project in crate::workweave_index::projects_on_disk(ctx.primary_path()) {
            if let Some(active) = receipt_scope {
                if project.as_str() != active {
                    continue;
                }
            }
            if let Ok(Some(path)) =
                crate::workspace::pending_index_migration(ctx.primary_path(), &project)
            {
                violations.push(CheckViolation::LegacyWorkweaveIndex {
                    project: project.clone(),
                    index_path: path,
                });
            }
        }
    }

    // §7.1's migration pass. Runs before the branch-discipline scan below so
    // a workweave it migrated reports as healthy rather than as both `[fixed]`
    // and a paired warning — the same ordering the reattach uses, for the same
    // reason.
    if fix {
        let fix_active = if scope_all {
            None
        } else {
            active_project_name.as_ref().map(|n| n.as_str())
        };
        let (applied, migration_errs) =
            fix_branch_model_migration(ctx.primary_path(), &git, fix_active, adopt_detached);
        for msg in &applied {
            println!("[fixed] core: {msg}");
        }
        for msg in migration_errs {
            all_issues_branch_discipline_errors.push(msg);
        }
    }

    // Ordering: the reattach runs first. Its condition (counterpart exists
    // and its tip equals HEAD) is read off state the deletion pass can
    // change, and a store that has just been reattached is no longer a
    // Detached finding — so reattaching first means the scan below reports
    // the state the operator is left in.
    if fix {
        if let Some(consent) = reattach {
            let fix_active = if scope_all {
                None
            } else {
                active_project_name.as_ref().map(|n| n.as_str())
            };
            let (reattached, reattach_errs) = fix_detached_canonicals(
                ctx.primary_path(),
                &git,
                &input.projects,
                fix_active,
                &input.known_repos,
                consent,
            );
            for (store_path, branch) in &reattached {
                println!(
                    "[fixed] core: reattached detached canonical {} to `{}`",
                    store_path.display(),
                    branch
                );
            }
            for msg in reattach_errs {
                all_issues_branch_discipline_errors.push(msg);
            }
        }
    }

    let mut branch_discipline_violations =
        scan_branch_discipline(ctx.primary_path(), &git, &input.projects);
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
        let (deleted, fix_errs) = fix_stale_ephemeral_branches(
            ctx.primary_path(),
            &git,
            &input.projects,
            fix_active,
            &input.known_repos,
        );
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

    for msg in registry_fix_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!("workweave-index --fix failed: {msg}"),
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

        // Content drift check: the integrations' `verify()` pass reports drift
        // between on-disk managed/generated content and what `activate()` would
        // produce. Under `--fix`, doctor invokes the intent-mode write path to
        // regenerate safe-to-fix drift. Without `--fix`, all drift findings
        // surface as warnings — `doctor` is the detector and the fixer.
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
            // `activate_intent_at` takes the weave dir as its parameter, so
            // one call covers both: `workspace_dir` is `ctx.active_path()`,
            // which IS `ctx.primary_path()` at primary and the workweave dir
            // inside a workweave.
            //
            // The weave the repair binds to is the ONLY thing that varies.
            // The workweave arm used to run a hook-suppressed variant, so a
            // missing `Cargo.lock` (a `generated_files()` entry only
            // `cargo generate-lockfile` can author) was reported as
            // safe-to-fix and then left missing by the fix that named itself
            // in the report.
            let result = crate::activate::activate_intent_at(project.name.as_str(), &workspace_dir);
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

        // Member-incompatibility observation arm. Informational and never
        // gated: an `Ownership::DefaultOnly` value the operator holds may be
        // incompatible with what the members require, which `verify()` does
        // NOT see — rule 5 keeps DefaultOnly divergence CLEAN, permanently, and
        // this coexists with that rather than reinterpreting it. The findings
        // are structurally `safe_to_fix = false` (no automated repair exists),
        // so they bypass the --fix partition above and always surface as-is.
        all_issues.extend(crate::integration_runner::run_member_incompatibilities(
            &integrations,
            &project.manifest,
            &ctx_base,
        ));

        // Framework-level Axis-1 surfacing check. Distinct from the
        // per-integration `verify()` pass above, which only sees Axis-2 content
        // drift: nothing there asserts that the *symlinks* the surfacing layer
        // should have created actually exist and resolve. It scopes to
        // `workspace_dir` (= `ctx.active_path()`), so run at primary it checks
        // primary's surfacing and run in a workweave it checks that workweave's.
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
            // this weave directory. It authors no content — it only (re)creates
            // the owner-scoped symlinks, which is valid in any weave (it is
            // exactly what workweave-create runs at creation).
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
/// Returns `{ "$schema": ..., "violations": [...], "plugins": [...],
/// "resolution"?: {...} }`. The `plugins` array carries the PATH inventory of
/// `rwv-*` executables (reporting only — never a failed check). `resolution`
/// is a pure projection of the resolved workspace context; absent when no
/// project is resolved.
pub fn build_doctor_json(
    violations: Vec<CheckViolation>,
    workspace_dir: &Path,
    workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
    resolution: Option<Resolution>,
    plugins: Vec<crate::plugins::PluginRecord>,
) -> serde_json::Value {
    let outputs: Vec<ViolationOutput> = violations
        .into_iter()
        .map(|v| ViolationOutput::from_violation(v, workspace_dir, workweave_dirs))
        .collect();
    let mut doc = serde_json::json!({
        "$schema": DOCTOR_SCHEMA_URL,
        "violations": outputs,
        "plugins": plugins,
    });
    if let Some(res) = resolution {
        doc["resolution"] = serde_json::to_value(res).unwrap_or(serde_json::Value::Null);
    }
    doc
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

    // The index-side twin (`branch-model.md` §7.1 arm 7). Same channel, same
    // scoping rule as the receipt findings below.
    for project in crate::workweave_index::projects_on_disk(ctx.primary_path()) {
        if !scope_all {
            if let Some(active) = active_project_name.as_ref() {
                if project.as_str() != active.as_str() {
                    continue;
                }
            }
        }
        if let Ok(Some(index_path)) =
            crate::workspace::pending_index_migration(ctx.primary_path(), &project)
        {
            violations.push(CheckViolation::LegacyWorkweaveIndex {
                project,
                index_path,
            });
        }
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
    // Dangling ownership receipts. Report-only here; the JSON channel never
    // auto-fixes. Scoped like the text channel.
    scan_dangling_receipts(
        &git,
        ctx.primary_path(),
        if scope_all {
            None
        } else {
            active_project_name.as_ref().map(|n| n.as_str())
        },
        &mut violations,
    );

    // Branch-discipline findings.
    // JSON channel never auto-fixes; `--fix` is reserved for `run_check`.
    // Scope: filter to active project unless scope_all (mirrors run_check).
    for v in scan_branch_discipline(ctx.primary_path(), &git, &input.projects) {
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
    // Discover `rwv-*` executables on PATH for the inventory. This is
    // reporting only — the presence or absence of plugins never affects the
    // has_violations signal or the doctor exit code.
    let plugins = crate::plugins::discover_plugins(None::<&std::ffi::OsStr>);
    let payload = build_doctor_json(
        violations,
        &workspace_dir,
        &workweave_dirs,
        ctx.resolution(),
        plugins,
    );
    let out =
        serde_json::to_string_pretty(&payload).context("failed to serialize doctor output")?;
    println!("{out}");
    Ok(has_violations)
}
