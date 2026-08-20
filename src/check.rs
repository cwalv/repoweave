//! Convention checks: orphaned clones, dangling refs, stale locks, index drift, working-tree drift, etc.
//!
//! `rwv doctor` builds a workspace-wide inventory from all projects, then runs
//! a series of checks. Integration check hooks are run separately.

use crate::git::RWV_MERGE_DRIVER_PREFIX;
use crate::integration::{Issue, IssueKind};
use crate::manifest::{LockFile, Manifest, Project, ProjectName, RepoPath, Role, WorkweaveName};
use crate::vcs::{ReplayExclusionState, ResolvedRevisionId};
use crate::workspace::{
    project_dir, project_rel_path, projects_dir, strip_projects_prefix, Resolution,
};
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
    /// A directory under a registry path not listed in any project's `rwv.toml`.
    OrphanedClone { path: RepoPath },

    /// An `rwv.toml` entry pointing to a path not present on disk.
    DanglingReference {
        project: ProjectName,
        repo: RepoPath,
    },

    /// An `rwv.toml` entry missing the `role` field.
    MissingRole {
        project: ProjectName,
        repo: RepoPath,
    },

    /// A project's `rwv.lock` doesn't match current HEAD SHAs.
    ///
    /// Reachable only for an entry the *resolved* lock carries, so this is not
    /// the whole of lock freshness: an entry whose repo is absent from disk is
    /// dropped before [`find_violations`] runs and is reported by
    /// [`run_check_locked`] instead. Both surfaces must name the same two
    /// revisions in the same spelling for every entry both of them see.
    StaleLock {
        project: ProjectName,
        repo: RepoPath,
        locked: ResolvedRevisionId,
        actual: ResolvedRevisionId,
    },

    /// An `rwv.toml` entry with no corresponding `rwv.lock` entry. This is a
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
    /// regenerate them. The committed-form check is the one sync's invariant
    /// reads; the migration commit is what makes it visible on the next
    /// rebase.
    MissingReplayExclusion {
        project: ProjectName,
        sub_kind: ReplayExclusionKind,
    },

    /// Reading a project repo's `.gitattributes` for the replay-exclusion
    /// entry failed, so the invariant is neither satisfied nor violated.
    ReplayExclusionUnreadable { project: ProjectName, error: String },

    /// A project repo does not define the `rwv-ours` merge driver in its own
    /// git config. `rwv sync` passes the definition per invocation, so its own
    /// rebase is unaffected; a bare `git rebase --continue` the operator runs
    /// afterwards is not, and git treats an undefined driver as `merge=binary`
    /// — conflict markers in `rwv.lock` where Phase 3 expects to regenerate.
    MissingMergeDriverConfig {
        project: ProjectName,
        config_key: String,
    },

    /// Reading a project repo's git config for the `rwv-ours` merge-driver
    /// definition failed, so the invariant is neither satisfied nor violated.
    MergeDriverConfigUnreadable {
        project: ProjectName,
        config_key: String,
        error: String,
    },

    /// A repo present under a registry directory whose HEAD `rwv doctor` could
    /// not read. Every freshness comparison for that repo is unevaluated, so
    /// suppressing it would leave the repo looking healthy.
    HeadUnreadable { repo: RepoPath, error: String },

    /// The `projects/` directory exists but `rwv doctor` could not list it —
    /// a permissions problem, most plausibly. Every project under it is
    /// invisible to this scan, which without this finding reads the same as
    /// a workspace that genuinely has none.
    ProjectsDirUnreadable { path: PathBuf, error: String },

    /// A directory under `projects/` holding no `rwv.toml` at any depth below
    /// it. It is not a project and contains none, so every enumeration walks
    /// past it in silence — which is what an operator who created it by hand
    /// reads as rwv having accepted it.
    ProjectlessDir { dir: PathBuf },

    /// A directory under `projects/` that holds an `rwv.toml` but whose path
    /// relative to `projects/` is not a name [`ProjectName::new`] accepts.
    /// The manifest is there and no verb can address it, because every verb
    /// takes the name through the validator first.
    ///
    /// Raised whatever `--project` narrows to: the derived name is one the
    /// validator refuses, so it can never equal a scope, and a scope filter
    /// would silence it everywhere instead of narrowing it.
    UnnameableProject {
        dir: PathBuf,
        derived: String,
        error: String,
    },

    /// An `rwv.lock` entry naming a revision the local clone cannot resolve —
    /// a lock written against history this clone has never fetched.
    UnresolvableLockEntry {
        project: ProjectName,
        repo: RepoPath,
    },

    /// A project directory holds the pre-TOML manifest and no
    /// [`Manifest::FILE_NAME`], so nothing in the project loads. Report-only:
    /// the file is hand-authored, and the comments and key order in it do not
    /// survive a mechanical cross-format rewrite.
    ///
    /// MIGRATORY finding: the pre-TOML manifest was written by rwv <=
    /// v0.16.0. The report arm is removable once every owned weave's health
    /// floor records a clean doctor at >= v0.18 (see
    /// [`crate::health_floor`]).
    LegacyManifestFormat {
        /// Project the manifest belongs to. Derived from the directory rather
        /// than read off a `Project`, which cannot load without a manifest
        /// rwv can parse.
        project: ProjectName,
        /// Absolute path to the pre-TOML manifest.
        legacy_path: PathBuf,
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

    /// A weave root carries BOTH `.rwv-active` and `.rwv-workweave`.
    ///
    /// The two files name the same fact — which project this tree belongs to
    /// — and are mutually exclusive by design, occupying one tier of the
    /// resolution chain rather than two: a primary root carries the pointer,
    /// a workweave root carries the marker. A tree holding both holds two
    /// copies of its own identity with nothing keeping them in agreement.
    ///
    /// Fixability turns on evidence held OUTSIDE the tree, because the two
    /// files are themselves the ambiguity — see
    /// [`WeaveRootIdentityConflictKind`].
    WeaveRootIdentityConflict {
        /// The weave root carrying both files.
        root: PathBuf,
        /// The project named by `.rwv-active`, when that file is non-empty.
        /// `None` for an empty or unreadable pointer, which is still a
        /// present file and still a conflict.
        pointer_project: Option<ProjectName>,
        /// Whether anything outside the tree settles its identity.
        sub_kind: WeaveRootIdentityConflictKind,
    },

    /// A `.rwv-workweave` marker file this build cannot use as-is: YAML
    /// (markers are JSON now), possibly also missing the `parent:` field
    /// required before the format changed. Auto-fixable: rewrite the file as
    /// JSON, backfilling `parent: <primary value>` where it is absent.
    LegacyWorkweaveMarker {
        /// Absolute path to the offending `.rwv-workweave` file.
        marker_path: PathBuf,
        /// The `primary:` value from the marker, used as the backfill value
        /// when `parent:` is itself absent.
        primary: PathBuf,
    },

    /// A `.rwv-workweave-index` written before ref-ownership receipts existed
    /// (no `receipts` field) — the index-side twin of
    /// [`LegacyWorkweaveMarker`](Self::LegacyWorkweaveMarker).
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

    /// A `.rwv-workweave-index` that exists but does not parse, so nothing
    /// derived from it is evaluated: the recorded placements, the ownership
    /// receipts, and the migration state
    /// ([`LegacyWorkweaveIndex`](Self::LegacyWorkweaveIndex)) alike.
    ///
    /// Reported so the state names itself. Without it every marker-bearing
    /// workweave in the project reads as
    /// [`UnregisteredWorkweave`](WorkweaveTreeIntegrityKind::UnregisteredWorkweave)
    /// — a finding whose repair reads the same file and dies on the same
    /// parse error, so the operator is handed a remedy that cannot run.
    ///
    /// No repair: the file is either hand-edited or truncated by a crashed
    /// writer, and rwv cannot tell which entries a corrupt one meant to hold.
    /// rwv's record of the generations it accepted for a project is present
    /// and does not parse as that record.
    ///
    /// Absence is not this: a weave that has never stamped has nothing to
    /// record, and the axes reading it are right to stay quiet. Bytes that are
    /// there and are not the record are different in kind — the managed-file
    /// drift and derived-state staleness checks both decide from it, both read
    /// an unparseable one as "nothing is attested", and both then report
    /// nothing. Without this finding that silence is indistinguishable from a
    /// clean project, including for files that had already drifted.
    ///
    /// No repair: rebuilding the record means re-deriving the generated files
    /// and attesting whatever results, and what was accepted before is exactly
    /// what has been lost, so nothing here can check one against the other.
    UnreadableOwnedState {
        /// The project whose record does not parse.
        project: ProjectName,
        /// Absolute path to the record.
        state_path: PathBuf,
        /// Rendered read/parse error.
        error: String,
    },

    UnreadableWorkweaveIndex {
        /// The project whose index does not parse.
        project: ProjectName,
        /// Absolute path to the index file.
        index_path: PathBuf,
        /// Rendered read/parse error chain.
        error: String,
    },

    /// A project directory exists but does not load: either its `rwv.toml`
    /// or its `rwv.lock` failed to parse.
    ///
    /// Reported as an `Error`-severity violation so the operator is not
    /// left with zero violations (i.e. an apparent "clean" result) for a
    /// project rwv cannot see into. `--fix` has no arm for it, and the
    /// remedy differs between the two files, so `message` carries the one
    /// the failing loader minted rather than this variant naming either.
    UnparseableProject {
        /// Relative project path (e.g. `my-app`, `org/repo`).
        project: ProjectName,
        /// Absolute path to the project's `rwv.toml`. Locates the project;
        /// the file that failed is named in `message` and is not always
        /// this one.
        manifest_path: PathBuf,
        /// Rendered error chain, remedy included. Free-form: no structured
        /// parse-error type survives to this boundary.
        message: String,
    },

    /// A `.rwv-workweave` marker tree anomaly: dangling parent, chain anomaly,
    /// unregistered directory, or foreign-primary marker.
    ///
    /// Whether `--fix` acts turns on the sub-kind, and the sub-kinds that
    /// carry no repair carry none because choosing one needs operator input.
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
    /// The `origin-url-mismatch` case needs the operator to decide whether
    /// the manifest or the remote is the source of truth; reference-role
    /// repos may intentionally diverge. The `lock-sha-unreachable` case needs
    /// a fetch from the remote, not a sync.
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
    /// `docs/explanation/joints/clone-topology.md`.
    ///
    /// All four sub-kinds are silent for every higher-tier `rwv doctor`
    /// check (those operate on revisions and content; this one operates on
    /// the physical object-store topology). Repair is an object-store
    /// migration — a re-parenting — which is out of `--fix` scope per the
    /// alpha guideline.
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
    /// age, the path, and the verb that started the op so the operator can
    /// inspect, resume, or roll back (`rwv abort`). **Never auto-fixed**:
    /// another terminal may be mid-conflict-resolution; rwv has no daemon to
    /// know which workspace the op-state legitimately belongs to.
    StaleOpState {
        /// Absolute path to the workspace dir that holds the `.rwv-op` file.
        workspace_dir: PathBuf,
        /// The verb that started the stalled op. `--continue` only resumes
        /// under the verb that started the op, so a report that omitted it
        /// could only guess which command to name.
        verb: crate::op_state::OpVerb,
        /// Raw `started_at` string from the op-state file (RFC3339 UTC),
        /// preserved verbatim so the operator sees the same value
        /// `op_state::read_owner` would.
        started_at: String,
    },

    /// A `.rwv-op-lease` file whose recorded owner workspace has no matching
    /// `.rwv-op` with the same op id — the **structural dead-lease** case.
    ///
    /// Unlike [`CheckViolation::StaleOpState`], this **is** auto-fixable: the classification
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

    /// An ownership receipt whose ref is not in the store it names — the benign residue of a crash between the
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
    /// *store* is gone is R4 territory — whether receipts are ever
    /// reclaimed in bulk under a store-destroy is open — so it is left alone
    /// here.
    DanglingRefReceipt {
        /// The project whose registry holds the receipt.
        project: ProjectName,
        /// Absolute path of the canonical store the receipt is keyed to.
        store_path: PathBuf,
        /// The recorded ref name that does not exist in that store.
        ref_name: String,
    },

    /// An ownership receipt whose ref name carries a `/` segment — a name
    /// no live workweave of that project mints, so a record that claims a
    /// ref rwv cannot have created, because every name rwv mints is flat.
    ///
    /// The ref itself usually **does** exist, which is what separates this
    /// from [`DanglingRefReceipt`](Self::DanglingRefReceipt): the residue is
    /// in the registry, not in the store. It is written on purpose, mid-flight,
    /// by the pre-flat migration — [`adopt_legacy`] then rename then
    /// retract — and it
    /// survives whenever that rename does not complete.
    ///
    /// Left in place it is worse than no receipt at all. The
    /// canonical-store pass asks which live workweave mints the recorded
    /// name; a segmented name is minted by
    /// none, so the ref reads as a *leak*, and holding a receipt is exactly
    /// what lifts a ref out of the untouchable Unowned class into the ones
    /// `--fix` deletes from. Where the ref is also checked out — the shape
    /// this arm exists for — every `--fix` re-attempts a deletion the VCS
    /// refuses, and doctor never converges.
    ///
    /// `--fix` retracts it through [`RefRegistry::retract`]. Retraction
    /// disowns; it does not touch the ref, so nothing is at risk, and the
    /// ref falls back to Unowned — reported, never auto-deleted.
    ///
    /// [`adopt_legacy`]: crate::workweave_index::RefRegistry::adopt_legacy
    /// [`RefRegistry::retract`]: crate::workweave_index::RefRegistry::retract
    PreFlatRefReceipt {
        /// The project whose registry holds the receipt.
        project: ProjectName,
        /// Absolute path of the canonical store the receipt is keyed to.
        store_path: PathBuf,
        /// The recorded ref name that carries a `/` segment.
        ref_name: String,
    },

    /// Two recorded sibling identities that differ only by ASCII case fold.
    ///
    /// A portability lint over the record, not an identity equivalence:
    /// rwv holds the two as distinct names and keeps resolving them
    /// byte-exactly. What it reports is that a filesystem which folds case
    /// cannot hold both, so a clone or fetch of this weave onto macOS or
    /// Windows collides — which is why it fires on case-sensitive hosts too,
    /// where the pair is perfectly legal and nothing else would notice.
    ///
    /// Report-only, and deliberately: renaming a recorded identity is the
    /// operator's call, and on the host that raised the warning nothing is
    /// broken yet.
    ///
    /// Residue: an ASCII fold does not see non-ASCII confusables (`ß`/`SS`,
    /// precomposed against decomposed). Those are the same class one size
    /// down, and only a filesystem that folds them reports them.
    ConfusableSiblings {
        /// The namespace the two names share, as an operator would name it.
        parent: String,
        /// The two spellings, ordered so the pair reads the same however the
        /// scan happened to find it.
        first: String,
        second: String,
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
    /// mandate versions across sovereign repos.
    CargoVersionSkew {
        /// The registry crate name (e.g. `serde`, `tokio`).
        crate_name: String,
        /// Per-member requirement strings, sorted for stable output.
        occurrences: Vec<crate::integrations::cargo_workspace::CargoSkewOccurrence>,
    },

    /// A member's `.cargo/config.toml` declares a `[patch.<registry>].<crate>`
    /// key that would silently defeat a weave-level entry for the same key
    /// (cargo's closest-config-wins per-key shadowing). Warning severity,
    /// report-only. Doubles as the mandatory precheck for derived-patch
    /// generation: cargo's mismatch diagnostic actively misleads (blames
    /// crates.io) when a patch silently doesn't apply, so surfacing the
    /// shadow at scan time preserves the operator's ability to diagnose the
    /// actual cause.
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
    /// (same as [`CheckViolation::DanglingReference`]), then re-run `rwv doctor` to verify.
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

    /// A `.gitattributes` line in a managed repo assigns an `rwv-`-prefixed
    /// merge driver that rwv does not define.
    ///
    /// The line reads like a working derived-content declaration and does
    /// nothing: git resolves `merge=<name>` through `merge.<name>.driver`
    /// config, and falls back to a textual 3-way merge — silently — when no
    /// config defines the name. The `rwv-` prefix is what makes this
    /// diagnosable rather than presumptuous: that namespace is rwv's, so a
    /// name inside it that rwv does not define is one nothing will ever
    /// define. Warning severity, report-only.
    PhantomMergeDriver {
        /// Manifest-relative path to the repo whose `.gitattributes` carries
        /// the line (a member repo, or `projects/<name>` for a project repo).
        repo: RepoPath,
        /// The path pattern the line assigns the driver to.
        pattern: String,
        /// The `rwv-`-prefixed driver name that resolves to nothing.
        driver: String,
    },
}

/// What `rwv doctor --fix` does about a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixDisposition {
    /// `--fix` repairs it with no further input.
    Auto,
    /// `--fix` repairs it only when the operator also passes this flag; bare
    /// `--fix` reports it and moves nothing.
    Consented(&'static str),
    /// `--fix` does not touch it.
    ReportOnly,
}

impl CheckViolation {
    /// The one place that says which findings `rwv doctor --fix` repairs.
    ///
    /// The match is exhaustive down to the sub-kind, so a finding added
    /// without a disposition does not compile. Every other statement of the
    /// set — the `--fix` flag help, `rwv explain doctor`, and the per-`kind`
    /// entries in `docs/reference/doctor-findings.md` — is checked against
    /// this one rather than maintained beside it.
    ///
    /// Answering here is not the same as repairing here: an
    /// [`Auto`](FixDisposition::Auto) finding may be repaired by a workspace
    /// pass that runs before collection, in which case the finding is never
    /// raised at all.
    pub fn fix_disposition(&self) -> FixDisposition {
        use FixDisposition::{Auto, Consented, ReportOnly};
        match self {
            CheckViolation::OrphanedClone { .. }
            | CheckViolation::DanglingReference { .. }
            | CheckViolation::MissingRole { .. }
            | CheckViolation::StaleLock { .. }
            | CheckViolation::IncompleteLock { .. }
            | CheckViolation::UnparseableProject { .. }
            | CheckViolation::LegacyManifestFormat { .. }
            | CheckViolation::WorkweaveDrift { .. }
            | CheckViolation::StaleOpState { .. }
            | CheckViolation::CargoVersionSkew { .. }
            | CheckViolation::CargoPatchShadowing { .. }
            | CheckViolation::MissingCanonicalClone { .. }
            | CheckViolation::UninitializedSubmodule { .. }
            | CheckViolation::ReplayExclusionUnreadable { .. }
            | CheckViolation::MergeDriverConfigUnreadable { .. }
            | CheckViolation::HeadUnreadable { .. }
            | CheckViolation::ProjectsDirUnreadable { .. }
            | CheckViolation::ProjectlessDir { .. }
            | CheckViolation::UnnameableProject { .. }
            | CheckViolation::UnresolvableLockEntry { .. }
            | CheckViolation::UnreadableOwnedState { .. }
            | CheckViolation::UnreadableWorkweaveIndex { .. }
            | CheckViolation::PhantomMergeDriver { .. } => ReportOnly,

            CheckViolation::MissingMergeDriverConfig { .. }
            | CheckViolation::DanglingActiveProject { .. }
            | CheckViolation::LegacyWorkweaveMarker { .. }
            | CheckViolation::LegacyWorkweaveIndex { .. }
            | CheckViolation::StaleWorktreeRegistration { .. }
            | CheckViolation::DanglingRefReceipt { .. }
            | CheckViolation::PreFlatRefReceipt { .. }
            | CheckViolation::DeadOpLease { .. } => Auto,

            CheckViolation::MissingReplayExclusion { sub_kind, .. } => match sub_kind {
                ReplayExclusionKind::Absent
                | ReplayExclusionKind::LegacySpelling
                | ReplayExclusionKind::LegacyAlongsideCurrent => Auto,
            },
            CheckViolation::IndexDrift { kind, .. } => match kind {
                IndexDriftKind::SafeToFix => Auto,
                IndexDriftKind::LiveStaged => ReportOnly,
            },
            CheckViolation::WorkingTreeDrift { kind, .. } => match kind {
                WorkingTreeDriftKind::SafeToFix => Auto,
                WorkingTreeDriftKind::LiveEdits => ReportOnly,
            },
            CheckViolation::WeaveRootIdentityConflict { sub_kind, .. } => match sub_kind {
                WeaveRootIdentityConflictKind::RegisteredWorkweave { .. } => Auto,
                WeaveRootIdentityConflictKind::MarkerUnverifiable { .. }
                | WeaveRootIdentityConflictKind::Unwitnessed { .. } => ReportOnly,
            },
            CheckViolation::WorkweaveTreeIntegrity { sub_kind, .. } => match sub_kind {
                WorkweaveTreeIntegrityKind::DanglingParent { .. }
                | WorkweaveTreeIntegrityKind::StaleRegistryEntry { .. }
                | WorkweaveTreeIntegrityKind::UnregisteredWorkweave { .. } => Auto,
                WorkweaveTreeIntegrityKind::ParentChainAnomaly { .. }
                | WorkweaveTreeIntegrityKind::UnregisteredDir
                | WorkweaveTreeIntegrityKind::ForeignPrimary { .. }
                | WorkweaveTreeIntegrityKind::ForeignPrimaryOtherWorkspace { .. }
                | WorkweaveTreeIntegrityKind::TrackedIndex { .. }
                | WorkweaveTreeIntegrityKind::UnreadableMarker { .. }
                | WorkweaveTreeIntegrityKind::NestedWorkweaveDir { .. }
                | WorkweaveTreeIntegrityKind::MisnamedDir { .. } => ReportOnly,
            },
            CheckViolation::Provenance { sub_kind, .. } => match sub_kind {
                ProvenanceKind::OriginUrlMismatch { .. }
                | ProvenanceKind::LockShaUnreachable { .. } => ReportOnly,
            },
            CheckViolation::CloneTopology { sub_kind, .. } => match sub_kind {
                CloneTopologyKind::StandaloneInWorkweave { .. }
                | CloneTopologyKind::DisconnectedWeaveClone { .. }
                | CloneTopologyKind::WrongParentWorktree { .. }
                | CloneTopologyKind::WeaveCloneIsWorktree { .. } => ReportOnly,
            },
            CheckViolation::BranchDiscipline { sub_kind, .. } => match sub_kind {
                BranchDisciplineKind::UnmigratedEphemeralBranch { .. }
                | BranchDisciplineKind::UnrecordedEphemeralBranch { .. }
                | BranchDisciplineKind::StaleEphemeralBranchSafe { .. } => Auto,
                BranchDisciplineKind::Detached { .. } => Consented("--adopt-detached-checkouts"),
                BranchDisciplineKind::CanonicalDetached { .. } => Consented("--reattach-checkouts"),
                BranchDisciplineKind::SharedBranch { .. }
                | BranchDisciplineKind::ForeignEphemeral { .. }
                | BranchDisciplineKind::BlockedEphemeralNamespace { .. }
                | BranchDisciplineKind::BlockedDetachedNamespace { .. }
                | BranchDisciplineKind::UnbornCheckout { .. }
                | BranchDisciplineKind::CanonicalHoldsLiveWorkweaveRef { .. }
                | BranchDisciplineKind::CanonicalHoldsLeakedRef { .. }
                | BranchDisciplineKind::StaleEphemeralBranchLive { .. }
                | BranchDisciplineKind::StaleEphemeralBranchUnowned { .. } => ReportOnly,
            },
            CheckViolation::OrphanedSavepoint { sub_kind, .. } => match sub_kind {
                OrphanedSavepointKind::Redundant => Auto,
                OrphanedSavepointKind::Live => ReportOnly,
            },
            CheckViolation::ConfusableSiblings { .. } => ReportOnly,
        }
    }

    /// The `kind` tag this violation serializes under in `rwv doctor --json`.
    ///
    /// [`ViolationOutput`] mirrors this enum variant-for-variant and tags
    /// with the kebab-cased variant name; this method states the same
    /// mapping for the internal type, so a filter can ask a violation's
    /// wire name without converting it. The agreement is pinned by a test
    /// that serializes every corpus violation and compares the emitted tag
    /// against this method — a variant added to one enum and not the other
    /// already fails to compile in [`ViolationOutput::from_violation`].
    pub fn wire_kind(&self) -> &'static str {
        match self {
            CheckViolation::OrphanedClone { .. } => "orphaned-clone",
            CheckViolation::DanglingReference { .. } => "dangling-reference",
            CheckViolation::MissingRole { .. } => "missing-role",
            CheckViolation::StaleLock { .. } => "stale-lock",
            CheckViolation::IncompleteLock { .. } => "incomplete-lock",
            CheckViolation::WorkweaveDrift { .. } => "workweave-drift",
            CheckViolation::IndexDrift { .. } => "index-drift",
            CheckViolation::WorkingTreeDrift { .. } => "working-tree-drift",
            CheckViolation::MissingReplayExclusion { .. } => "missing-replay-exclusion",
            CheckViolation::ReplayExclusionUnreadable { .. } => "replay-exclusion-unreadable",
            CheckViolation::MissingMergeDriverConfig { .. } => "missing-merge-driver-config",
            CheckViolation::MergeDriverConfigUnreadable { .. } => "merge-driver-config-unreadable",
            CheckViolation::HeadUnreadable { .. } => "head-unreadable",
            CheckViolation::ProjectsDirUnreadable { .. } => "projects-dir-unreadable",
            CheckViolation::ProjectlessDir { .. } => "projectless-dir",
            CheckViolation::UnnameableProject { .. } => "unnameable-project",
            CheckViolation::UnresolvableLockEntry { .. } => "unresolvable-lock-entry",
            CheckViolation::LegacyManifestFormat { .. } => "legacy-manifest-format",
            CheckViolation::DanglingActiveProject { .. } => "dangling-active-project",
            CheckViolation::WeaveRootIdentityConflict { .. } => "weave-root-identity-conflict",
            CheckViolation::LegacyWorkweaveMarker { .. } => "legacy-workweave-marker",
            CheckViolation::LegacyWorkweaveIndex { .. } => "legacy-workweave-index",
            CheckViolation::UnreadableOwnedState { .. } => "unreadable-owned-state",
            CheckViolation::UnreadableWorkweaveIndex { .. } => "unreadable-workweave-index",
            CheckViolation::UnparseableProject { .. } => "unparseable-project",
            CheckViolation::WorkweaveTreeIntegrity { .. } => "workweave-tree-integrity",
            CheckViolation::Provenance { .. } => "provenance",
            CheckViolation::CloneTopology { .. } => "clone-topology",
            CheckViolation::BranchDiscipline { .. } => "branch-discipline",
            CheckViolation::StaleWorktreeRegistration { .. } => "stale-worktree-registration",
            CheckViolation::StaleOpState { .. } => "stale-op-state",
            CheckViolation::DeadOpLease { .. } => "dead-op-lease",
            CheckViolation::DanglingRefReceipt { .. } => "dangling-ref-receipt",
            CheckViolation::PreFlatRefReceipt { .. } => "pre-flat-ref-receipt",
            CheckViolation::OrphanedSavepoint { .. } => "orphaned-savepoint",
            CheckViolation::ConfusableSiblings { .. } => "confusable-siblings",
            CheckViolation::CargoVersionSkew { .. } => "cargo-version-skew",
            CheckViolation::CargoPatchShadowing { .. } => "cargo-patch-shadowing",
            CheckViolation::MissingCanonicalClone { .. } => "missing-canonical-clone",
            CheckViolation::UninitializedSubmodule { .. } => "uninitialized-submodule",
            CheckViolation::PhantomMergeDriver { .. } => "phantom-merge-driver",
        }
    }
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

/// Which spelling of the replay exclusion the project repo carries, which
/// decides whether `--fix` writes the entry fresh or migrates one in place.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayExclusionKind {
    /// `.gitattributes` carries no entry for `rwv.lock` at all.
    Absent,
    /// `.gitattributes` carries the legacy `merge=ours` spelling. The driver
    /// was renamed to close a collision with a global-config `ours` driver;
    /// the old name reads as the invariant being met while sync's check —
    /// which matches the current name — sees nothing.
    LegacySpelling,
    /// `.gitattributes` carries both spellings for `rwv.lock`. Which one git
    /// applies is decided by reading order, and the legacy name is live
    /// either way: a global `merge.ours.driver` binds to it during a bare
    /// `git rebase --continue`.
    LegacyAlongsideCurrent,
}

/// The finding a project's replay-exclusion state raises, `None` for the one
/// state that raises none.
///
/// Doctor decides this once. The scan turns the answer into a finding and
/// `--fix` turns it into a repair, so `--fix` cannot act on a state the scan
/// called clean — the two read one classification of one read of the file
/// rather than each deriving its own.
fn replay_exclusion_finding(state: ReplayExclusionState) -> Option<ReplayExclusionKind> {
    match state {
        ReplayExclusionState::Current => None,
        ReplayExclusionState::Absent => Some(ReplayExclusionKind::Absent),
        ReplayExclusionState::LegacyOnly => Some(ReplayExclusionKind::LegacySpelling),
        ReplayExclusionState::LegacyAlongsideCurrent => {
            Some(ReplayExclusionKind::LegacyAlongsideCurrent)
        }
    }
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

/// Discriminator for [`CheckViolation::WeaveRootIdentityConflict`] findings:
/// whether anything outside the tree settles which of its two identity files
/// is the true one.
///
/// The split is not symmetric, and deliberately so. The naive reading — "a
/// workweave's stray pointer is safe to delete, a primary's stray marker is
/// not" — cannot be implemented, because it presumes we already know which
/// kind of root this is, and the marker's presence is the only witness of
/// that. Primary-ness has no independent signature: a primary root and a
/// workweave root both hold `projects/` and registry directories. So the
/// question "which file is the stray?" is exactly the question the conflict
/// makes unanswerable from the tree alone, and the discriminator has to come
/// from somewhere else.
///
/// The registry is that somewhere else. It lives at
/// `<primary>/projects/<project>/.rwv-workweave-index`, is written only by
/// `rwv workweave create`, and records the absolute path of every workweave
/// it made. A tree the registry names is a workweave on the authority of a
/// file the tree does not contain and could not have forged by being copied.
///
/// Note what is deliberately NOT used as the discriminator: whether the tree
/// itself contains a `.rwv-workweave-index`. That looks like a primary-ness
/// signature and is not one. The index is untracked, so whether a workweave
/// inherits a copy depends on whether its `projects/<project>/` is a linked
/// worktree (it is not copied) or a plain directory copy (it is) — a
/// topology accident, not a fact about identity. Keying on it would classify
/// real workweaves as unwitnessed in the copy topology and leave their stray
/// pointers unfixable.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum WeaveRootIdentityConflictKind {
    /// The marker names THIS workspace's primary, and that primary's registry
    /// for the marker's project records THIS exact directory. External
    /// evidence settles it: the tree is a workweave, so `.rwv-active` is the
    /// redundant copy and deleting it destroys nothing the marker and the
    /// registry do not already say. Auto-fixable — `--fix` deletes the
    /// pointer and leaves the marker.
    RegisteredWorkweave {
        /// Project the marker names (and under whose registry it is recorded).
        project: String,
        /// Name the registry records this directory under.
        workweave_name: String,
    },
    /// The marker itself cannot witness the identity it claims — unreadable,
    /// legacy (YAML, or missing `parent:`), or naming a `primary:` that
    /// verifies as no workspace at all. `observe_root` classifies a root like this
    /// `MarkerUnverifiable` rather than `Disputed` even with `.rwv-active`
    /// present alongside: a marker that cannot prove its own claim cannot
    /// prove which of the two files is the stray either, so this is
    /// report-only for the same reason `Unwitnessed` is. Never auto-fixed —
    /// repairing the marker (`rwv doctor --fix` migrates a legacy one; a
    /// dangling or unreadable one needs a hand edit) is a separate step from
    /// clearing a pointer whose redundancy the marker cannot yet vouch for.
    MarkerUnverifiable {
        /// Absolute path to the `.rwv-workweave` file.
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
        marker_path: PathBuf,
        /// Why the marker cannot witness its own claim.
        defect: crate::workspace::MarkerDefect,
    },
    /// The marker is readable and verifies, but names a different primary,
    /// or names this primary with no registry entry pointing back at this
    /// directory. Report-only. Deleting either file here would be a guess,
    /// and the wrong guess destroys operator state — the marker in
    /// particular carries `primary` and `parent` values that exist nowhere
    /// else.
    ///
    /// The most likely cause of the last shape is a workweave copied
    /// out-of-band (`cp -r`): the copy carries both files, and the registry
    /// still names only the original.
    Unwitnessed {
        /// Why no external evidence was found, in operator-facing terms.
        detail: String,
    },
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
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
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
    /// scan was started from, and the path itself resolves to no workspace
    /// either (missing, or exists but is not a workspace root) — e.g. an
    /// rsync'd workweave whose marker still points at the origin machine's
    /// absolute path. Report-only.
    ForeignPrimary {
        /// The primary path recorded in the marker (unresolved).
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
        marker_primary: PathBuf,
    },
    /// The marker's `primary:` path does not match this workspace, but
    /// resolves to a different, valid workspace root — the normal shape
    /// when several weaves share one workweave container. Not a defect in
    /// this workweave, so excluded from the default text report: every
    /// sibling weave's doctor would otherwise repeat this about every other
    /// sibling. Still enumerated under `--json`.
    ForeignPrimaryOtherWorkspace {
        /// The other workspace's primary path (resolved).
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
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
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
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
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
        index_path: PathBuf,
    },
    /// A `.rwv-workweave` marker that parses as neither current JSON nor a
    /// `migrate_legacy`-repairable legacy shape — YAML with no `primary:`
    /// for `migrate_legacy` to backfill from, or no valid `project:` for it
    /// to construct from. Every marker rwv has ever written carries all
    /// three fields, so this is hand-corruption or a truncated write rather
    /// than a shape upgrading produces. Report-only: there is no value here
    /// to guess a repair from.
    UnreadableMarker {
        /// Why the marker cannot be read, and what to write in its place.
        detail: String,
    },
    /// A recorded workweave whose directory lies *below* a workweave
    /// container rather than directly in it. A multi-segment project name
    /// used to render its `/` through into the directory name, so
    /// `chatly/web-app` + `wtest` placed the workweave at
    /// `<container>/chatly/web-app--wtest` and left `chatly` behind as a
    /// directory the container scan reads as a stray.
    ///
    /// The scan enumerates a container's immediate children, so nothing else
    /// in this check sees such a directory at all; this finding is emitted
    /// from the registry side, which records the path.
    ///
    /// Report-only, and the remedy is retire-and-recreate rather than a
    /// rename: the move crosses a directory boundary, which invalidates the
    /// worktree back-pointers of every checkout inside and the recorded path
    /// that found it. Workweaves are ephemeral by design; rwv does not
    /// migrate a live seat in place.
    NestedWorkweaveDir {
        /// Project the entry belongs to.
        project: String,
        /// The recorded name of the workweave.
        workweave_name: String,
        /// The single-segment directory name the records spell today.
        expected_dir_name: String,
    },
    /// A marker-bearing workweave directory whose basename disagrees with
    /// its records: it does not spell `{marker project}--{name}`, where the
    /// name is the one the project's registry records for this path.
    ///
    /// Only a hand-rename produces this — `rwv workweave create` derives
    /// the directory name from the same (project, name) pair it writes into
    /// the marker and the registry. Identity is by record, so the scans keep
    /// working from the records; what this finding reports is that the
    /// directory's own name now lies about them, which misleads operators
    /// and collides with any future workweave whose records genuinely mint
    /// this basename. Where no registry entry names the path there is no
    /// recorded name to disagree with, and the question narrows to whether
    /// the basename is one the marker's project could have rendered at all:
    /// when it is not, identity is unrecoverable and the scans skip the
    /// directory entirely — this finding is then the only signal.
    ///
    /// Report-only: renaming the directory back is the operator's one-step
    /// remedy (the checkouts inside were registered under the recorded
    /// name, so restoring it also restores their worktree back-pointers),
    /// and when no record exists the intended name is not derivable from
    /// the directory itself.
    MisnamedDir {
        /// The basename the records expect (`{project}--{name}`), when the
        /// records pin one. `None` when no registry entry names this path
        /// and the basename is not one the marker's project renders.
        expected_dir_name: Option<String>,
        /// What disagrees: which half, and with which record.
        detail: String,
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
        /// The URL recorded in the manifest (`rwv.toml`).
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
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
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
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
        weave_store_path: PathBuf,
        /// Absolute path of a representative store one of the workweave
        /// checkouts actually uses (the one this weave clone is
        /// disconnected from).
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
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
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
        expected_store_path: PathBuf,
        /// Absolute path of the canonical store this workweave checkout
        /// is actually linked into.
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
        actual_store_path: PathBuf,
    },
    /// The weave path `<weave>/<repo_path>` itself is a linked worktree of
    /// some other clone — full inversion: there is no canonical store at
    /// the manifest slot, and the workspace there shares its DAG with
    /// whichever clone hosts the actual store.
    WeaveCloneIsWorktree {
        /// Absolute path of the canonical store this slot is linked into.
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
        actual_store_path: PathBuf,
    },
}
/// Discriminator for [`CheckViolation::BranchDiscipline`] findings.
///
/// Three groupings, mirroring the three checks in the spec:
///
/// * (a) workweave-branch — a workweave checkout is on the wrong branch, or
///   on a ref of its own namespace that predates the flat naming:
///   [`SharedBranch`](Self::SharedBranch),
///   [`ForeignEphemeral`](Self::ForeignEphemeral),
///   [`Detached`](Self::Detached),
///   [`UnmigratedEphemeralBranch`](Self::UnmigratedEphemeralBranch),
///   [`UnrecordedEphemeralBranch`](Self::UnrecordedEphemeralBranch),
///   [`UnbornCheckout`](Self::UnbornCheckout).
/// * (b) canonical-store attachment — what the canonical store's HEAD is:
///   [`CanonicalHoldsLiveWorkweaveRef`](Self::CanonicalHoldsLiveWorkweaveRef),
///   [`CanonicalHoldsLeakedRef`](Self::CanonicalHoldsLeakedRef),
///   [`CanonicalDetached`](Self::CanonicalDetached).
/// * (c) stale-ephemeral-branches — a `<project>--<name>/...` branch
///   exists in a canonical clone but workweave `<name>` no longer exists
///   on disk: [`StaleEphemeralBranchSafe`](Self::StaleEphemeralBranchSafe),
///   [`StaleEphemeralBranchLive`](Self::StaleEphemeralBranchLive), or
///   [`StaleEphemeralBranchUnowned`](Self::StaleEphemeralBranchUnowned).
///   The safe/live split applies the doctrine in
///   `docs/explanation/joints/shared-refs-drift.md` to refs: a tip that is an
///   ancestor of the primary's tracking-branch tip carries no unique work and
///   is safely removable; a tip with commits not reachable from the primary
///   is live work and must be left alone.
///
/// # Ownership is by record, never by name shape (R2)
///
/// The (b) grouping and the safe/live/unowned split in (c) both key on
/// whether rwv holds a persisted ownership receipt
/// ([`crate::workweave_index::RefRegistry`]) for the exact ref in the exact
/// store. A branch that merely *looks* like one of rwv's — a hand-made
/// `<a>--<b>/<c>` — is an operator branch: the canonical-store pass leaves
/// it alone, and `--fix` never deletes it.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum BranchDisciplineKind {
    /// (a) The workweave checkout is on a non-ephemeral branch (e.g. `main`).
    ///
    /// Caused by `git switch main` inside a workweave or by a bare clone
    /// that was never moved to an ephemeral branch. The fixture for this
    /// sub-kind exercises the bare-main-in-workweave case from the spec's
    /// acceptance criteria: the violation must flag from creation, before
    /// any commit lands.
    ///
    /// Report-only, deliberately — not a missing arm. The state is
    /// operator-made (a `git switch` run by hand), unlike the fetch-written
    /// detachments whose repairs are native consented arms, and the
    /// remediation prints the exact registry-aware `git switch` (it names
    /// an existing recorded branch whenever a receipt exists), which is
    /// what keeps hand-running it safe. If measured recurrence reopens the
    /// question, the native form is a targeted repair naming one checkout,
    /// not a bulk consent flag.
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
    /// both are report-only, so the distinction costs nothing but accuracy,
    /// and report-only is deliberate for the same reason as
    /// [`SharedBranch`](Self::SharedBranch)'s: the state is operator-made
    /// and the printed switch is exact.
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
    /// directly at a commit instead of a named branch. With no branch
    /// name there is nothing for the merged-check to ask about and
    /// nothing for the workweave's ref namespace to be keyed by, so both
    /// invariants lapse for as long as the checkout stays detached.
    ///
    /// `--fix --adopt-detached-checkouts` mints the workweave's flat ref
    /// **at HEAD** — i.e. at the lock SHA — and, when `legacy_branch` is
    /// `Some`, gives that branch's name up to make room for it.
    Detached {
        /// The ephemeral ref this workweave mints (`<project>--<workweave>`).
        expected_ref: String,
        /// See [`SharedBranch`](Self::SharedBranch)'s field of the same
        /// name.
        recorded_ref: Option<String>,
        /// The commit HEAD names directly.
        at_sha: String,
        /// A pre-flat branch of this workweave's own namespace, with its
        /// tip. **Both** tips are reported, because they are the two things
        /// the operator is choosing between.
        legacy_branch: Option<LegacyRefAtTip>,
    },
    /// (a) The workweave checkout is attached to a pre-flat `<project>--<workweave>/<segment>` ref of its **own**
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
    /// (a) Two or more refs share this workweave's namespace in one store, so
    /// the flat name cannot be created and no migration arm can run.
    ///
    /// git holds `refs/heads/p--w` and `refs/heads/p--w/x` as a file and a
    /// directory of the same name, so the rename the migration would perform
    /// is refused whatever order the arms take. `fix_branch_model_migration`
    /// skips the pair before any arm, which is why this is reported in place
    /// of [`UnmigratedEphemeralBranch`](Self::UnmigratedEphemeralBranch)
    /// rather than beside it: that finding's message promises a rename this
    /// state cannot produce.
    ///
    /// Report-only, and the repair is an operator's judgement rather than a
    /// missing arm — which of the refs is this workweave's branch, and where
    /// the others belong, is not derivable from the refs themselves.
    BlockedEphemeralNamespace {
        /// The flat ref no arm can create while the namespace is shared
        /// (`<project>--<workweave>`).
        expected_ref: String,
        /// Every pre-flat ref found under that namespace, in listing order.
        blocking_refs: Vec<String>,
    },
    /// (a) The workweave checkout is in detached-HEAD state AND two or more
    /// refs share this workweave's namespace, so `--adopt-detached-checkouts`
    /// cannot run.
    ///
    /// `fix_branch_model_migration` skips the whole repo before any arm when
    /// `legacy_refs.len() > 1` — including the consented detached arm — which
    /// is why this is reported in place of
    /// [`Detached`](Self::Detached) rather than beside it: that finding's
    /// message promises `--adopt-detached-checkouts`, a flag whose arm the
    /// guard prevents from running. The principle is consent-tier-independent:
    /// a consented remedy that cannot run misleads the operator exactly as an
    /// auto remedy does — consent changes who acts, not whether the named
    /// action works.
    ///
    /// Report-only. The operator must reduce the namespace to at most one ref,
    /// then re-run to get the ordinary [`Detached`](Self::Detached) finding
    /// with a remedy that will actually run.
    BlockedDetachedNamespace {
        /// The flat ref that cannot be created while the namespace is shared
        /// (`<project>--<workweave>`).
        expected_ref: String,
        /// The commit HEAD names directly.
        at_sha: String,
        /// Every pre-flat ref found under that namespace, in listing order.
        blocking_refs: Vec<String>,
    },
    /// (a) The workweave's flat ref exists in the canonical store, but rwv
    /// holds no receipt for it.
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
    /// (a) The workweave checkout is on a branch with no commits.
    ///
    /// Report-only, and not because a fix is missing: there is no revision
    /// to record a receipt against, so there is nothing the migration could
    /// own. `rwv lock` is where an unborn HEAD is actionable.
    UnbornCheckout {
        /// The branch HEAD points at, which has no commits yet.
        branch: String,
    },
    /// (b) The canonical store is attached to a ref rwv recorded as
    /// belonging to a workweave that is **still on disk**.
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
    /// (b) The canonical store is attached to a ref rwv recorded as
    /// belonging to a workweave that is **gone** — a leak.
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
        /// Not the workweave: rwv does not try to reconstruct which
        /// workweave a stray ref belonged to. The receipt
        /// records `(store, name, created_at)`, and the workweave is
        /// recoverable only while one on disk would mint that name — which
        /// is exactly the case this variant is *not*.
        project: String,
    },
    /// (b) The canonical store — or the project repo — is in detached-HEAD
    /// state.
    ///
    /// The project repo is an instance of the branch model, so it is
    /// checked here rather than exempted.
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
        /// Whether the reattach condition holds: `counterpart` exists as a
        /// local branch **and** its tip equals HEAD.
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
        /// rwv does not reconstruct which workweave a ref belonged to, and
        /// for this class no workweave on disk would
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
    /// (c) A branch shaped like one rwv minted before the naming scheme was
    /// flattened, sitting in a canonical store, which **rwv holds no
    /// ownership receipt for** and which no workweave on disk claims.
    ///
    /// Under R2 this ref is not rwv's: name shape is not ownership. It is
    /// reported so the operator can see it, and it is never deleted —
    /// deleting this class is how a hand-made `<a>--<b>/<c>` branch can
    /// disappear under `--fix`.
    ///
    /// # Why this one is discovered by shape and nothing else is
    ///
    /// Every other arm asks the registry or asks a live workweave's
    /// **minted** name. This arm has neither to ask: there is no receipt, and
    /// reconstructing which workweave the ref belonged to is forbidden — so
    /// the alternative to a shape heuristic is not a better signal, it is
    /// silence, and the refs the operator most needs to see (the pre-receipt
    /// population the migration cannot reach) would simply stop being
    /// reported.
    ///
    /// What keeps that sound is that the heuristic yields a `bool` and
    /// nothing else — see `looks_like_a_pre_flat_ref`. No name is taken
    /// apart, no workweave is named, and the only route to a DESTROY runs
    /// through an `OwnedRef`, which only a persisted receipt produces. A
    /// false positive costs one line of output and can cost nothing more.
    StaleEphemeralBranchUnowned {
        /// The full branch name.
        branch: String,
    },
}

/// A pre-flat branch and the commit it reaches. **Both** tips are reported,
/// side by side, because the operator is choosing between them.
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
        #[serde(rename = "sub_kind")]
        sub_kind: ReplayExclusionKind,
    },
    ReplayExclusionUnreadable {
        project: String,
        error: String,
    },
    MissingMergeDriverConfig {
        project: String,
        config_key: String,
    },
    MergeDriverConfigUnreadable {
        project: String,
        config_key: String,
        error: String,
    },
    HeadUnreadable {
        path: String,
        absolute_path: String,
        error: String,
    },
    ProjectsDirUnreadable {
        path: String,
        error: String,
    },
    ProjectlessDir {
        absolute_path: String,
    },
    UnnameableProject {
        absolute_path: String,
        derived: String,
        error: String,
    },
    UnresolvableLockEntry {
        path: String,
        absolute_path: String,
        project: String,
    },
    LegacyManifestFormat {
        project: String,
        legacy_path: String,
    },
    DanglingActiveProject {
        project: String,
        missing_dir: String,
    },
    WeaveRootIdentityConflict {
        /// Absolute path of the weave root carrying both identity files.
        root: String,
        /// The project named by `.rwv-active`; absent when that file is
        /// empty or unreadable.
        pointer_project: Option<String>,
        #[serde(rename = "sub_kind")]
        sub_kind: WeaveRootIdentityConflictKind,
    },
    LegacyWorkweaveMarker {
        marker_path: String,
        primary: String,
    },
    LegacyWorkweaveIndex {
        project: String,
        index_path: String,
    },
    UnreadableOwnedState {
        project: String,
        state_path: String,
        error: String,
    },
    UnreadableWorkweaveIndex {
        project: String,
        index_path: String,
        error: String,
    },
    UnparseableProject {
        project: String,
        manifest_path: String,
        /// Free-form display string of the parse error. Named `message`
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
        /// The verb that started the stalled op — the one `--continue`
        /// resumes it under.
        verb: crate::op_state::OpVerb,
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
        /// Observability-only — never a decision input.
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
    /// See [`CheckViolation::PreFlatRefReceipt`].
    PreFlatRefReceipt {
        /// The project whose registry holds the receipt.
        project: String,
        /// Absolute path of the canonical store the receipt is keyed to.
        store_path: String,
        /// The recorded ref name that carries a `/` segment.
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
    /// See [`CheckViolation::ConfusableSiblings`].
    ConfusableSiblings {
        /// The namespace the two names share.
        parent: String,
        /// The two spellings, in a stable order.
        first: String,
        second: String,
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

    /// See [`CheckViolation::PhantomMergeDriver`].
    PhantomMergeDriver {
        /// Manifest-relative path to the repo carrying the `.gitattributes`.
        path: String,
        /// Absolute path to that repo on disk.
        absolute_path: String,
        /// The path pattern the offending line assigns the driver to.
        pattern: String,
        /// The `rwv-`-prefixed driver name that resolves to nothing.
        driver: String,
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
            crate::path_spelling::wire_path(&workspace_dir.join(repo.as_path()))
        }
        fn abs_in(
            workweave: &Option<WorkweaveName>,
            workspace_dir: &Path,
            workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
            repo: &RepoPath,
        ) -> String {
            let root = workweave
                .as_ref()
                .and_then(|ww| workweave_dirs.get(ww))
                .map(std::path::PathBuf::as_path)
                .unwrap_or(workspace_dir);
            crate::path_spelling::wire_path(&root.join(repo.as_path()))
        }

        match violation {
            CheckViolation::OrphanedClone { path } => Self::OrphanedClone {
                absolute_path: abs(workspace_dir, &path),
                path: path.to_string(),
            },
            CheckViolation::ConfusableSiblings {
                parent,
                first,
                second,
            } => Self::ConfusableSiblings {
                parent,
                first,
                second,
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
                    absolute_path: crate::path_spelling::wire_path(
                        &dir_for_ww.join(repo.as_path()),
                    ),
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
            CheckViolation::MissingReplayExclusion { project, sub_kind } => {
                Self::MissingReplayExclusion {
                    project: project.to_string(),
                    sub_kind,
                }
            }
            CheckViolation::ReplayExclusionUnreadable { project, error } => {
                Self::ReplayExclusionUnreadable {
                    project: project.to_string(),
                    error,
                }
            }
            CheckViolation::MissingMergeDriverConfig {
                project,
                config_key,
            } => Self::MissingMergeDriverConfig {
                project: project.to_string(),
                config_key,
            },
            CheckViolation::MergeDriverConfigUnreadable {
                project,
                config_key,
                error,
            } => Self::MergeDriverConfigUnreadable {
                project: project.to_string(),
                config_key,
                error,
            },
            CheckViolation::HeadUnreadable { repo, error } => Self::HeadUnreadable {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                error,
            },
            CheckViolation::ProjectsDirUnreadable { path, error } => Self::ProjectsDirUnreadable {
                path: crate::path_spelling::wire_path(&path),
                error,
            },
            CheckViolation::ProjectlessDir { dir } => Self::ProjectlessDir {
                absolute_path: crate::path_spelling::wire_path(&dir),
            },
            CheckViolation::UnnameableProject {
                dir,
                derived,
                error,
            } => Self::UnnameableProject {
                absolute_path: crate::path_spelling::wire_path(&dir),
                derived,
                error,
            },
            CheckViolation::UnresolvableLockEntry { project, repo } => {
                Self::UnresolvableLockEntry {
                    absolute_path: abs(workspace_dir, &repo),
                    path: repo.to_string(),
                    project: project.to_string(),
                }
            }
            CheckViolation::LegacyManifestFormat {
                project,
                legacy_path,
            } => Self::LegacyManifestFormat {
                project: project.to_string(),
                legacy_path: crate::path_spelling::wire_path(&legacy_path),
            },
            CheckViolation::DanglingActiveProject {
                project,
                missing_dir,
            } => Self::DanglingActiveProject {
                project: project.to_string(),
                missing_dir: crate::path_spelling::wire_path(&missing_dir),
            },
            CheckViolation::WeaveRootIdentityConflict {
                root,
                pointer_project,
                sub_kind,
            } => Self::WeaveRootIdentityConflict {
                root: crate::path_spelling::wire_path(&root),
                pointer_project: pointer_project.map(|p| p.to_string()),
                sub_kind,
            },
            CheckViolation::LegacyWorkweaveMarker {
                marker_path,
                primary,
            } => Self::LegacyWorkweaveMarker {
                marker_path: crate::path_spelling::wire_path(&marker_path),
                primary: crate::path_spelling::wire_path(&primary),
            },
            CheckViolation::LegacyWorkweaveIndex {
                project,
                index_path,
            } => Self::LegacyWorkweaveIndex {
                project: project.to_string(),
                index_path: crate::path_spelling::wire_path(&index_path),
            },
            CheckViolation::UnreadableOwnedState {
                project,
                state_path,
                error,
            } => Self::UnreadableOwnedState {
                project: project.to_string(),
                state_path: crate::path_spelling::wire_path(&state_path),
                error,
            },
            CheckViolation::UnreadableWorkweaveIndex {
                project,
                index_path,
                error,
            } => Self::UnreadableWorkweaveIndex {
                project: project.to_string(),
                index_path: crate::path_spelling::wire_path(&index_path),
                error,
            },
            CheckViolation::UnparseableProject {
                project,
                manifest_path,
                message,
            } => Self::UnparseableProject {
                project: project.to_string(),
                manifest_path: crate::path_spelling::wire_path(&manifest_path),
                message,
            },
            CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir,
                sub_kind,
            } => Self::WorkweaveTreeIntegrity {
                workweave_dir: crate::path_spelling::wire_path(&workweave_dir),
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
                absolute_path: crate::path_spelling::wire_path(&workspace_path),
                path: repo.to_string(),
                sub_kind,
            },
            CheckViolation::BranchDiscipline {
                repo_path,
                sub_kind,
            } => Self::BranchDiscipline {
                repo_path: crate::path_spelling::wire_path(&repo_path),
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
                missing_path: crate::path_spelling::wire_path(&missing_path),
            },
            CheckViolation::StaleOpState {
                workspace_dir: ws_dir,
                verb,
                started_at,
            } => Self::StaleOpState {
                workspace_dir: crate::path_spelling::wire_path(&ws_dir),
                verb,
                started_at,
            },
            CheckViolation::DeadOpLease {
                workspace_dir: ws_dir,
                op_id,
                recorded_owner,
                sub_kind,
                created_at,
            } => Self::DeadOpLease {
                workspace_dir: crate::path_spelling::wire_path(&ws_dir),
                op_id,
                recorded_owner: crate::path_spelling::wire_path(&recorded_owner),
                sub_kind,
                created_at,
            },
            CheckViolation::DanglingRefReceipt {
                project,
                store_path,
                ref_name,
            } => Self::DanglingRefReceipt {
                project: project.to_string(),
                store_path: crate::path_spelling::wire_path(&store_path),
                ref_name,
            },
            CheckViolation::PreFlatRefReceipt {
                project,
                store_path,
                ref_name,
            } => Self::PreFlatRefReceipt {
                project: project.to_string(),
                store_path: crate::path_spelling::wire_path(&store_path),
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
                weave_config: crate::path_spelling::wire_path(&weave_config),
                member_config: crate::path_spelling::wire_path(&member_config),
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
                    absolute_path: crate::path_spelling::wire_path(&ww_dir.join(repo.as_path())),
                    path: repo.to_string(),
                    workweave: workweave.to_string(),
                    canonical_path: crate::path_spelling::wire_path(&canonical_path),
                }
            }
            CheckViolation::UninitializedSubmodule {
                workweave,
                repo,
                empty_paths,
            } => {
                let ww_dir = workweave_dirs.get(&workweave);
                let absolute_path = crate::path_spelling::wire_path(
                    &ww_dir
                        .unwrap_or(&workspace_dir.to_path_buf())
                        .join(repo.as_path()),
                );
                Self::UninitializedSubmodule {
                    absolute_path,
                    path: repo.to_string(),
                    workweave: workweave.as_str().to_string(),
                    empty_paths,
                }
            }
            CheckViolation::PhantomMergeDriver {
                repo,
                pattern,
                driver,
            } => Self::PhantomMergeDriver {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                pattern,
                driver,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// IssueOutput — wire-format mirror of Issue for `--json`
// ---------------------------------------------------------------------------
//
// Same split as `CheckViolation` -> `ViolationOutput`, and for the same
// reason: `Issue` is the integration contract's type, carried across a trait
// boundary third-party integrations implement, and the wire format is owned
// here. Nothing in `crate::integration` derives serde.

/// [`crate::integration::Severity`] on the wire.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SeverityOutput {
    Warning,
    Error,
}

/// [`IssueKind`] on the wire.
///
/// Externally tagged, which is the shape the findings page already documents
/// for `sub_kind`: a kind with no fields of its own is a plain string, and one
/// that carries fields is a single-key object whose key is the tag. The tags
/// are [`IssueKind::tag`]'s, and a divergence between the two is what
/// `IssueKindOutput::from_kind` is exhaustive to prevent.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum IssueKindOutput {
    ToolMissing,
    ManagedFileMissing,
    ManagedFileDrift,
    ManagedFileUserHeld,
    Surfacing,
    ConfigRejected,
    MemberIncompatibility(MemberIncompatibilityOutput),
    DerivedStateStale,
    DisabledIntegrationArtifact,
    IntegrationFailed,
    CoreFinding,
}

/// The four facts a `member-incompatibility` predicate established, as fields
/// rather than as the sentence they are also rendered into.
#[derive(Debug, Serialize, JsonSchema)]
pub struct MemberIncompatibilityOutput {
    /// The managed file holding the incompatible value.
    pub path: String,
    /// Display form of the `DefaultOnly` key.
    pub key: String,
    /// The value currently on disk.
    pub on_disk: String,
    /// The strongest value the members require.
    pub required: String,
    /// The member file carrying that requirement.
    pub required_by: String,
}

impl IssueKindOutput {
    fn from_kind(kind: IssueKind) -> Self {
        match kind {
            IssueKind::ToolMissing => Self::ToolMissing,
            IssueKind::ManagedFileMissing => Self::ManagedFileMissing,
            IssueKind::ManagedFileDrift => Self::ManagedFileDrift,
            IssueKind::ManagedFileUserHeld => Self::ManagedFileUserHeld,
            IssueKind::Surfacing => Self::Surfacing,
            IssueKind::ConfigRejected => Self::ConfigRejected,
            IssueKind::MemberIncompatibility(observation) => {
                Self::MemberIncompatibility(MemberIncompatibilityOutput {
                    path: observation.path().to_string_lossy().into_owned(),
                    key: observation.key().to_owned(),
                    on_disk: observation.on_disk().to_owned(),
                    required: observation.required().to_owned(),
                    required_by: observation.required_by().to_owned(),
                })
            }
            IssueKind::DerivedStateStale => Self::DerivedStateStale,
            IssueKind::DisabledIntegrationArtifact => Self::DisabledIntegrationArtifact,
            IssueKind::IntegrationFailed => Self::IntegrationFailed,
            IssueKind::CoreFinding => Self::CoreFinding,
        }
    }
}

/// One integration-reported finding as it appears in `rwv doctor --json`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct IssueOutput {
    pub kind: IssueKindOutput,
    /// The integration that raised it, or `core` for a finding raised by
    /// `rwv doctor` itself while driving the integrations.
    pub integration: String,
    pub severity: SeverityOutput,
    /// Operator-facing prose. Everything a consumer routes on is a field —
    /// matching on this string is what `kind` exists to replace.
    pub message: String,
    /// Whether `rwv doctor --fix` is permitted to auto-repair this finding.
    /// `false` marks a user-held file region auto-repair would destroy.
    pub safe_to_fix: bool,
}

impl IssueOutput {
    /// Convert an internal [`Issue`] into its wire-format counterpart.
    pub fn from_issue(issue: Issue) -> Self {
        Self {
            kind: IssueKindOutput::from_kind(issue.kind),
            integration: issue.integration,
            severity: match issue.severity {
                crate::integration::Severity::Warning => SeverityOutput::Warning,
                crate::integration::Severity::Error => SeverityOutput::Error,
            },
            message: issue.message,
            safe_to_fix: issue.safe_to_fix,
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy-manifest-format scanning
// ---------------------------------------------------------------------------

/// One project directory holding a pre-TOML manifest and no
/// [`Manifest::FILE_NAME`].
#[derive(Debug, Clone)]
pub struct LegacyManifestFile {
    pub project: ProjectName,
    pub legacy_path: PathBuf,
}

/// Walk every project directory under `workspace_dir` for a manifest left in
/// the pre-TOML format.
///
/// Runs instead of, not alongside, a parse: a project whose only manifest is
/// the legacy one has nothing `Project::from_dir` can open, so without this
/// scan it reads as a directory with no manifest and is passed over in
/// silence. Which is also why it walks the tree itself rather than taking
/// [`crate::workspace::discover_projects`]'s answer: what it looks for is
/// precisely a directory that enumeration cannot see.
pub fn scan_workspace_for_legacy_manifests(workspace_dir: &Path) -> Vec<LegacyManifestFile> {
    let projects_root = projects_dir(workspace_dir);
    let mut found = Vec::new();
    scan_project_dir_for_legacy(&projects_root, &projects_root, &mut found);
    found
}

/// Recursively walk a project directory in `projects/`. Project names are
/// derived as the path relative to `projects/` (so
/// `projects/chatly/web-app/` yields project name `chatly/web-app`),
/// matching the nested-project convention `Project::from_dir` uses.
///
/// A directory carrying both names is not reported: rwv reads the one it
/// understands, and the leftover is the operator's to remove.
fn scan_project_dir_for_legacy(
    projects_dir: &Path,
    project_dir: &Path,
    out: &mut Vec<LegacyManifestFile>,
) {
    let legacy_path = project_dir.join(Manifest::LEGACY_FILE_NAME);
    if legacy_path.is_file() && !project_dir.join(Manifest::FILE_NAME).is_file() {
        let project_name = project_dir
            .strip_prefix(projects_dir)
            .unwrap_or(project_dir)
            .to_string_lossy()
            .into_owned();
        if let Ok(project) = ProjectName::new(project_name) {
            out.push(LegacyManifestFile {
                project,
                legacy_path,
            });
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

// ---------------------------------------------------------------------------
// Legacy-workweave-marker scanning and fixing
// ---------------------------------------------------------------------------

/// One workweave directory whose `.rwv-workweave` file is a legacy (YAML)
/// marker rather than the current JSON shape.
#[derive(Debug, Clone)]
pub struct LegacyWorkweaveMarkerFile {
    /// Absolute path to the `.rwv-workweave` file.
    pub marker_path: PathBuf,
    /// The `primary:` value read from the file (used as the backfill value
    /// when the file's `parent:` is itself absent).
    pub primary: PathBuf,
}

/// Walk the workweave parent directory and collect `.rwv-workweave` files
/// this build cannot use as-is.
///
/// A marker is "legacy" if it parses as YAML but not as the current JSON
/// shape — with or without `parent:` present. A directory whose marker fails
/// to parse at all, or whose legacy marker has no `primary:` of its own to
/// report, is not included — both are
/// `crate::workspace::legacy_marker_primary`'s call, the single parse of
/// `.rwv-workweave` behind this scan.
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
            if let Some(primary) = crate::workspace::legacy_marker_primary(&dir) {
                found.push(LegacyWorkweaveMarkerFile {
                    marker_path: crate::workspace::WorkweaveMarker::path_in(&dir),
                    primary,
                });
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
/// Findings for marker-bearing directories one level below `dir`, which is
/// itself a marker-less child of a workweave container.
///
/// That shape is what a multi-segment project name used to produce: the `/`
/// rendered through into the directory name, so the workweave landed a level
/// down and its first segment was left behind as a directory carrying no
/// marker. The container scan reads immediate children only, so without this
/// the workweave is invisible and the leftover segment is reported as a stray
/// an operator is told to remove — advice that would take a live workweave
/// with it.
///
/// Empty when `dir` holds no marker-bearing child, which is the ordinary
/// stray directory the caller reports instead.
fn nested_workweave_findings(ws_root: &Path, dir: &Path) -> Vec<CheckViolation> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let child = entry.path();
        if !child.is_dir() {
            continue;
        }
        let Ok(Some(marker)) = crate::workspace::WorkweaveMarker::read(&child) else {
            continue;
        };
        let basename = child.file_name().map(|n| n.to_string_lossy().into_owned());
        let name = crate::workweave::workweave_name_for_path(ws_root, marker.project(), &child)
            .ok()
            .flatten()
            .or_else(|| {
                basename
                    .as_deref()
                    .and_then(|b| crate::workspace::workweave_name_in(marker.project(), b))
            });
        let Some(name) = name else {
            continue;
        };
        found.push(CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: child.clone(),
            sub_kind: WorkweaveTreeIntegrityKind::NestedWorkweaveDir {
                project: marker.project().as_str().to_string(),
                workweave_name: name.as_str().to_string(),
                expected_dir_name: crate::workspace::weave_dir_name(marker.project(), &name),
            },
        });
    }
    found
}

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
    for project in crate::workspace::discover_projects(ws_root) {
        if let Ok(Some(idx)) = crate::workweave_index::read(ws_root, &project) {
            push_unique(idx.container, &mut containers);
        }
    }
    containers
}

/// Rewrite a legacy `.rwv-workweave` file as JSON, backfilling
/// `parent: <primary>` where `parent:` is absent.
///
/// Idempotent: if the file is already a JSON marker, it is not rewritten.
/// Returns `true` if the file was rewritten, `false` if it was already
/// up to date.
pub fn fix_legacy_workweave_marker(finding: &LegacyWorkweaveMarkerFile) -> anyhow::Result<bool> {
    let dir = finding
        .marker_path
        .parent()
        .expect(".rwv-workweave marker path always has a parent directory");
    crate::workspace::WorkweaveMarker::migrate_legacy(dir)
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
    if marker.parent().as_path().exists() {
        return Ok(false);
    }

    marker.repoint_parent(primary);
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
/// [`crate::vcs::Vcs::set_replay_exclusion`] during the legacy `merge=ours` →
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
///
/// MIGRATORY arm: repairs the legacy replay-exclusion spelling written by
/// rwv <= v0.12.1. Removable once every owned weave's health floor records
/// a clean doctor at >= v0.18 (see [`crate::health_floor`]).
pub(crate) fn commit_replay_exclusion_migration(
    vcs: &dyn crate::vcs::Vcs,
    project_dir: &Path,
) -> anyhow::Result<CommitOutcome> {
    const ATTRIBUTES_FILE: &str = ".gitattributes";

    // Read what is already staged BEFORE touching the index, so unrelated
    // work is never swept into an rwv-authored commit.
    let already_staged = vcs
        .staged_paths(project_dir)
        .with_context(|| format!("failed to read staged paths in {}", project_dir.display()))?;
    if already_staged.iter().any(|p| p != ATTRIBUTES_FILE) {
        return Ok(CommitOutcome::SkippedUnrelatedStaged);
    }

    vcs.stage_paths(project_dir, &[ATTRIBUTES_FILE])
        .with_context(|| {
            format!(
                "failed to stage {ATTRIBUTES_FILE} in {}",
                project_dir.display()
            )
        })?;

    let staged = vcs.has_staged_changes(project_dir).with_context(|| {
        format!(
            "failed to check staged changes in {}",
            project_dir.display()
        )
    })?;
    if !staged {
        return Ok(CommitOutcome::NothingToCommit);
    }

    vcs.commit(
        project_dir,
        "chore: migrate rwv.lock merge=ours → merge=rwv-ours (rwv doctor --fix)",
    )
    .with_context(|| format!("failed to commit in {}", project_dir.display()))?;
    Ok(CommitOutcome::Committed)
}

// ---------------------------------------------------------------------------
// Workweave-tree integrity scanning
// ---------------------------------------------------------------------------

/// Reconcile every project's `.rwv-workweave-index` against on-disk state.
///
/// Emits four registry-specific finding kinds:
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
/// * `unreadable-workweave-index` — the file exists and does not parse. This
///   is the read that decides the other three, so it is also the read that
///   reports its own failure: a project whose index does not parse is
///   excluded from pass 2 rather than having every workweave on disk
///   reported as `unregistered-workweave`, whose repair reads this same file.
///
/// The scan iterates every project's recorded container, so a project that
/// moved its container keeps reconciliation coverage. A workweave placed
/// outside every recorded container — what `create --dir` does — keeps none:
/// pass 2 enumerates containers, and nothing walks the host behind it, so
/// such a directory is reported by no finding here and adopted by no `--fix`.
/// That limit is what
/// [`crate::workspace::WorkweaveNameRecord::require`]'s refusal states
/// instead of promising around. Bootstrapping workspaces (no index
/// yet, live workweaves at the compiled-in default) surface every
/// marker-bearing directory as `unregistered-workweave` — the intended
/// self-heal path is `rwv doctor --fix` on first run after upgrade.
/// One finding per project whose accepted-generation record is present and
/// unreadable.
///
/// Per project rather than per generated file: the record that would name the
/// attested files is the one that cannot be read.
fn scan_unreadable_owned_state(ws_root: &Path) -> Vec<CheckViolation> {
    crate::workspace::discover_projects(ws_root)
        .into_iter()
        .filter_map(|project| {
            let project_dir = crate::workspace::project_dir(ws_root, project.as_str());
            crate::owned_state::unreadable_ledger(&project_dir).map(|error| {
                CheckViolation::UnreadableOwnedState {
                    state_path: crate::owned_state::ledger_path(&project_dir),
                    project,
                    error,
                }
            })
        })
        .collect()
}

fn scan_registry_reconciliation(vcs: &dyn crate::vcs::Vcs, ws_root: &Path) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    // Pass 1 — every recorded entry that fails validation is stale. Also
    // collect the set of validated (project, canonical-path) pairs so
    // pass 2 can identify unregistered orphans without double-reporting
    // ones the operator already recorded.
    let mut recorded_valid_paths: std::collections::HashSet<(String, PathBuf)> =
        std::collections::HashSet::new();
    let mut unreadable_index_projects: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for project in crate::workspace::discover_projects(ws_root) {
        let index = match crate::workweave_index::read(ws_root, &project) {
            Ok(Some(idx)) => Some(idx),
            Ok(None) => None,
            Err(e) => {
                unreadable_index_projects.insert(project.as_str().to_string());
                violations.push(CheckViolation::UnreadableWorkweaveIndex {
                    project: project.clone(),
                    index_path: crate::workweave_index::index_path(ws_root, &project),
                    error: format!("{e:#}"),
                });
                None
            }
        };
        for (name, path) in index.iter().flat_map(|idx| idx.workweaves.iter()) {
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
        if index_path.exists() && is_tracked_in_parent_repo(vcs, &index_path) {
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
    // container so per-workweave overrides are covered. A project whose index
    // did not parse has no validated set to be absent from, so "orphan" is
    // not a fact about it — `unreadable-workweave-index` above is.
    let mut seen_orphans: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for container in workweave_containers_for_scan(ws_root) {
        for (project, name, dir) in crate::workweave::doctor_scan_container(ws_root, &container) {
            if unreadable_index_projects.contains(project.as_str()) {
                continue;
            }
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

/// Best-effort check for whether `path` is tracked by the repo containing it.
///
/// Asks from the file's parent directory, so a path whose parent is not a
/// repo at all answers `false` rather than escaping upward. An unreachable
/// git is `false` too — hygiene surfaces should never fabricate findings on
/// non-git-managed projects.
fn is_tracked_in_parent_repo(vcs: &dyn crate::vcs::Vcs, path: &Path) -> bool {
    let dir = match path.parent() {
        Some(d) => d,
        None => return false,
    };
    let name = match path.file_name() {
        Some(n) => n,
        None => return false,
    };
    vcs.is_tracked(dir, Path::new(name)).unwrap_or(false)
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
/// Scan for weave roots carrying BOTH `.rwv-active` and `.rwv-workweave`.
///
/// `.rwv-active` and `.rwv-workweave` occupy one tier of the resolution
/// chain, not two: a primary root carries the pointer, a workweave root
/// carries the marker, never both. rwv itself no longer writes a pointer into
/// a workweave root, so a tree holding both got there some other way — a hand
/// edit, an out-of-band directory copy, or a workweave created by a build
/// from before the exclusivity rule. This scan is what makes the rule
/// enforced rather than merely intended.
///
/// **Which roots are inspected.** `primary_root` itself, `active_path` (the
/// weave the invocation resolved into, which is the only way a tree outside
/// every recorded container gets looked at), and every directory in every
/// container recorded for this workspace. Deduplicated by canonical path, so
/// one `rwv doctor` at primary covers the whole workspace in a single pass.
///
/// **Which arm class this is.** Workspace-rooted: the scan starts from
/// `primary_root` unconditionally and repairs whichever tree holds the
/// conflict, so `--fix` run inside workweave A will clear a stray pointer in
/// sibling workweave B. That is the same scoping the registry, dangling-parent
/// and dangling-active-project arms already have, and for the same reason —
/// the evidence that classifies a tree (the registry) lives only at primary,
/// so there is no per-weave view of it to bind to.
fn scan_weave_root_identity(primary_root: &Path, active_path: &Path) -> Vec<CheckViolation> {
    let mut roots: Vec<PathBuf> = vec![primary_root.to_path_buf(), active_path.to_path_buf()];
    for container in workweave_containers_for_scan(primary_root) {
        if let Ok(entries) = std::fs::read_dir(&container) {
            for e in entries.flatten() {
                if e.path().is_dir() {
                    roots.push(e.path());
                }
            }
        }
    }

    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut violations = Vec::new();
    for root in roots {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if let Some(v) = classify_weave_root_identity(primary_root, &canonical) {
            violations.push(v);
        }
    }
    violations.sort_by_key(|v| match v {
        CheckViolation::WeaveRootIdentityConflict { root, .. } => root.clone(),
        _ => PathBuf::new(),
    });
    violations
}

/// Classify one candidate root: `None` when it is not carrying both files.
///
/// `observe_root` is the same reader `resolve` consumes, so this and
/// resolution cannot diverge on what a root *is*, and the two arms a pointer
/// beside a marker produces are the two this reports on. Neither turns on what
/// either file parses to: an empty pointer or a legacy marker is still a
/// present file and still two copies of one fact.
fn classify_weave_root_identity(primary_root: &Path, root: &Path) -> Option<CheckViolation> {
    use crate::workspace::{observe_root, ActivePointer, RootObservation};

    let (pointer_project, marker) = match observe_root(root)? {
        RootObservation::MarkerUnverifiable {
            marker_path,
            defect,
            pointer: ActivePointer::Present(pointer_project),
            ..
        } => {
            return Some(CheckViolation::WeaveRootIdentityConflict {
                root: root.to_path_buf(),
                pointer_project,
                sub_kind: WeaveRootIdentityConflictKind::MarkerUnverifiable {
                    marker_path,
                    defect,
                },
            })
        }
        RootObservation::Disputed {
            marker, pointer, ..
        } => (pointer, marker),
        _ => return None,
    };

    let unwitnessed = |detail: String| CheckViolation::WeaveRootIdentityConflict {
        root: root.to_path_buf(),
        pointer_project: pointer_project.clone(),
        sub_kind: WeaveRootIdentityConflictKind::Unwitnessed { detail },
    };

    if !marker.names_primary(primary_root) {
        return Some(unwitnessed(format!(
            "The marker names primary `{}`, which is not this workspace, so this \
             workspace's registry has no say over it.",
            marker.primary().display()
        )));
    }

    // The registry entry is the external witness: it lives at
    // `<primary>/projects/<project>/.rwv-workweave-index`, is written only by
    // `rwv workweave create`, and names this directory by absolute path.
    let recorded = crate::workweave_index::read(primary_root, marker.project())
        .ok()
        .flatten()
        .and_then(|idx| {
            idx.workweaves
                .into_iter()
                .find(|(_, path)| path.canonicalize().unwrap_or_else(|_| path.clone()) == *root)
        });

    match recorded {
        Some((workweave_name, _)) => Some(CheckViolation::WeaveRootIdentityConflict {
            root: root.to_path_buf(),
            pointer_project,
            sub_kind: WeaveRootIdentityConflictKind::RegisteredWorkweave {
                project: marker.project().to_string(),
                workweave_name,
            },
        }),
        None => Some(unwitnessed(format!(
            "The marker names project `{}` of this workspace, but that project's registry \
             does not record this directory — most likely a workweave copied out-of-band \
             (`cp -r`), whose registry entry still names the original.",
            marker.project()
        ))),
    }
}

/// Recorded sibling identities that differ only by ASCII case fold, across
/// the two namespaces the record owns: project names and repo-path segments.
///
/// Project names are enumerated by **walking `projects/` per directory**
/// rather than through `discover_project_paths`, which reads only the
/// immediate children and so cannot see a multi-segment project at all — a
/// lint that inherited that would silently skip exactly the nested namespaces
/// this weave now spells. A listing also answers the question directly: a case
/// fold is about entries sharing one parent, which is what each directory
/// hands back at every depth. Repo paths come from each manifest's own
/// `repositories` keys, grouped by parent — that record needs no disk at all.
fn scan_confusable_siblings(ws_root: &Path, projects: &[Project]) -> Vec<CheckViolation> {
    fn walk(dir: &Path, label: &str, out: &mut Vec<CheckViolation>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut names = Vec::new();
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            names.push(entry.file_name().to_string_lossy().into_owned());
            subdirs.push(entry.path());
        }
        out.extend(
            crate::workspace::confusable_siblings(label, &names)
                .into_iter()
                .map(|pair| CheckViolation::ConfusableSiblings {
                    parent: pair.parent,
                    first: pair.first,
                    second: pair.second,
                }),
        );
        for sub in subdirs {
            let Some(leaf) = sub.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            walk(&sub, &format!("{label}/{leaf}"), out);
        }
    }

    let mut violations = Vec::new();
    // The label is read off the directory the layout owner names, so the
    // segment keeps its single spelling.
    let projects_root = crate::workspace::projects_dir(ws_root);
    let root_label = projects_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    walk(&projects_root, &root_label, &mut violations);

    for project in projects {
        let mut by_parent: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for repo_path in project.manifest.repositories.keys() {
            let spelled = repo_path.to_string();
            let (parent, leaf) = match spelled.rsplit_once('/') {
                Some((parent, leaf)) => (parent.to_owned(), leaf.to_owned()),
                None => (String::new(), spelled.clone()),
            };
            by_parent.entry(parent).or_default().push(leaf);
        }
        for (parent, names) in by_parent {
            let label = if parent.is_empty() {
                format!("project `{}`", project.name.as_str())
            } else {
                format!("project `{}` at {parent}", project.name.as_str())
            };
            violations.extend(
                crate::workspace::confusable_siblings(&label, &names)
                    .into_iter()
                    .map(|pair| CheckViolation::ConfusableSiblings {
                        parent: pair.parent,
                        first: pair.first,
                        second: pair.second,
                    }),
            );
        }
    }
    violations.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    violations.dedup_by(|a, b| format!("{a:?}") == format!("{b:?}"));
    violations
}

pub fn scan_workweave_tree_integrity(
    vcs: &dyn crate::vcs::Vcs,
    ws_root: &Path,
) -> Vec<CheckViolation> {
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
    violations.extend(scan_registry_reconciliation(vcs, ws_root));
    violations.extend(scan_unreadable_owned_state(ws_root));

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
        let marker_path = crate::workspace::WorkweaveMarker::path_in(dir);

        if !marker_path.exists() {
            let nested = nested_workweave_findings(ws_root, dir);
            if nested.is_empty() {
                violations.push(CheckViolation::WorkweaveTreeIntegrity {
                    workweave_dir: dir.clone(),
                    sub_kind: WorkweaveTreeIntegrityKind::UnregisteredDir,
                });
            } else {
                violations.extend(nested);
            }
            continue;
        }

        // Try to parse the marker. A marker `migrate_legacy` can repair is
        // handled by the separate legacy-workweave-marker check; we skip it
        // here (it gets a `LegacyWorkweaveMarker` violation instead, which
        // directs the operator to `--fix`). Anything else broken about the
        // marker is not that check's to report, so it is this one's.
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
                if let Some(detail) = crate::workspace::unmigratable_marker_detail(dir) {
                    violations.push(CheckViolation::WorkweaveTreeIntegrity {
                        workweave_dir: dir.clone(),
                        sub_kind: WorkweaveTreeIntegrityKind::UnreadableMarker { detail },
                    });
                }
                continue;
            }
        };

        // Foreign-primary check: marker's `primary` must resolve to ws_root.
        if !marker.names_primary(&ws_canonical) {
            let marker_primary_canonical = marker.primary_resolved();
            let sub_kind =
                if crate::workspace::is_workspace_root(marker_primary_canonical.as_path()) {
                    WorkweaveTreeIntegrityKind::ForeignPrimaryOtherWorkspace {
                        marker_primary: marker_primary_canonical.into_path_buf(),
                    }
                } else {
                    WorkweaveTreeIntegrityKind::ForeignPrimary {
                        marker_primary: marker.primary().to_path_buf(),
                    }
                };
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: dir.clone(),
                sub_kind,
            });
            // A foreign-primary marker's `parent` field refers to another
            // machine's paths; chain analysis against our on-disk tree would
            // produce noise. Skip further checks for this directory.
            continue;
        }

        // Dangling-parent check: the parent path must exist on disk.
        if !marker.parent().as_path().exists() {
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: dir.clone(),
                sub_kind: WorkweaveTreeIntegrityKind::DanglingParent {
                    parent_path: marker.parent().as_path().to_path_buf(),
                },
            });
            // Even with a dangling parent we can still collect the entry
            // for the cycle/cross-project check using what we have.
        }

        // Misnamed-dir check: the basename must spell what the records say.
        // Identity is by record — project from the marker, name from the
        // registry entry naming this path — so a disagreement here never
        // shifts what the scans validate; it reports that the directory's
        // name now lies about the records it carries. Without a registry
        // entry there is no name to disagree with, and all that is left to
        // ask is whether the basename is one the marker's project could have
        // rendered at all.
        if let Some(basename) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) {
            let recorded =
                crate::workweave::workweave_name_for_path(ws_root, marker.project(), dir)
                    .ok()
                    .flatten();
            match recorded {
                Some(name) => {
                    let expected_dir = crate::workspace::weave_dir_name(marker.project(), &name);
                    if basename != expected_dir {
                        violations.push(CheckViolation::WorkweaveTreeIntegrity {
                            workweave_dir: dir.clone(),
                            sub_kind: WorkweaveTreeIntegrityKind::MisnamedDir {
                                expected_dir_name: Some(expected_dir.clone()),
                                detail: format!(
                                    "the marker records project `{}` and the name the registry \
                                     records for this path is `{name}`, so the records expect \
                                     `{expected_dir}`",
                                    marker.project().as_str(),
                                ),
                            },
                        });
                    }
                }
                None if crate::workspace::workweave_name_in(marker.project(), &basename)
                    .is_none() =>
                {
                    violations.push(CheckViolation::WorkweaveTreeIntegrity {
                        workweave_dir: dir.clone(),
                        sub_kind: WorkweaveTreeIntegrityKind::MisnamedDir {
                            expected_dir_name: None,
                            detail: format!(
                                "the basename is not a directory name project `{p}` renders \
                                 for any workweave name, and no registry entry of project \
                                 `{p}` names this path, so the intended name is not derivable",
                                p = marker.project().as_str(),
                            ),
                        },
                    });
                }
                None => {}
            }
        }

        marker_entries.push(MarkerEntry {
            dir: dir.clone(),
            project: marker.project().clone(),
            parent: marker.parent().as_path().to_path_buf(),
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
    use crate::vcs::vcs_for;

    let mut violations = Vec::new();

    for project in projects {
        // --- origin-url-mismatch ---
        for (repo_path, entry) in project.manifest.iter_entries() {
            let vcs = vcs_for(entry.vcs_type);
            let repo_abs = workspace_dir.join(repo_path.as_path());
            if !repo_abs.is_dir() {
                continue;
            }

            let manifest_url = entry.url.to_string();
            let actual_url = match vcs.remote_url(&repo_abs) {
                Ok(Some(u)) => u,
                Ok(None) => continue, // no remote recorded — not this check's concern
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
            // The lock names paths; the manifest names backends. A lock entry
            // with no manifest entry has no declared backend to resolve from.
            let vcs = project
                .manifest
                .get_entry(repo_path)
                .map(|e| vcs_for(e.vcs_type))
                .unwrap_or_else(crate::vcs::probe_vcs);
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
            let sha_to_test = match vcs.resolve_revision(&repo_abs, lock_entry.version.as_str()) {
                Ok(resolved) => resolved.as_str().to_owned(),
                Err(_) => lock_entry.version.as_str().to_owned(),
            };

            match vcs.commit_object_exists(&repo_abs, &sha_to_test) {
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
// Phantom merge-driver scanning
// ---------------------------------------------------------------------------

/// `true` when `name` is a merge driver rwv can define.
///
/// Answered against rwv's own vocabulary — the names this code knows how to
/// define — and deliberately NOT against `merge.<name>.driver` in the repo's
/// config. rwv supplies that definition two ways: durably, via
/// [`crate::git::plant_rwv_merge_driver_config`], and per-invocation, via the
/// `-c` flags a stated derived-content policy contributes to a single git
/// command. The second leaves nothing on disk to detect, so a config probe
/// would answer "no such driver" for a name rwv defines every time it replays,
/// and the verdict would turn on which commands happened to run last in that
/// repo. The static answer holds in every repo at every moment, which is what
/// a check the operator is meant to act on needs.
fn rwv_defines_merge_driver(name: &str) -> bool {
    name == crate::git::RWV_MERGE_DRIVER_NAME
}

/// Scan managed repos for `.gitattributes` lines that assign an
/// `rwv-`-prefixed merge driver rwv does not define.
///
/// A `<path> merge=<driver>` line is inert unless some config git can see
/// defines `merge.<driver>.driver`; git falls back to an ordinary textual
/// 3-way merge and says nothing about it. Under the `rwv-` prefix that
/// fallback is permanent (see `RWV_MERGE_DRIVER_PREFIX`), so the line is a
/// declaration that reads as working and never will. Naming it is the point
/// of this scan.
///
/// **Only that direction.** A derived path carrying no attribute is NOT a
/// finding: which paths a repo declares derived is the repo's own business
/// (D1 — declaration is per-repo, opt-in). The single standing exception
/// predates this scan and stays exactly where it is: `rwv.lock`, which
/// `rwv init` writes and `rwv doctor --fix` repairs.
///
/// Reads the working-tree `.gitattributes`, like
/// [`Vcs::replay_exclusion_state`](crate::vcs::Vcs::replay_exclusion_state): the
/// operator's next commit is the one worth catching, and a file read costs no
/// subprocess. Report-only — repairing a phantom means guessing whether the
/// operator meant `rwv-ours` or meant nothing at all.
///
/// Each repo is scanned once even when several projects list it.
pub fn scan_phantom_merge_drivers(
    workspace_dir: &Path,
    projects: &[Project],
) -> Vec<CheckViolation> {
    let mut violations = Vec::new();
    let mut scanned: BTreeSet<RepoPath> = BTreeSet::new();

    for project in projects {
        // The project repo carries `.gitattributes` too — it is where the
        // lock's own declaration lives — and is not a manifest entry.
        let project_repo = RepoPath::new(project_rel_path(project.name.as_str())).ok();
        for repo in project_repo
            .iter()
            .chain(project.manifest.iter_repo_paths())
        {
            if !scanned.insert(repo.clone()) {
                continue;
            }
            let attrs = workspace_dir.join(repo.as_path()).join(".gitattributes");
            // Absent (the common case) or unreadable: nothing is declared
            // here, which this check has no opinion about.
            let Ok(contents) = std::fs::read_to_string(&attrs) else {
                continue;
            };
            for (pattern, driver) in phantom_merge_drivers_in(&contents) {
                violations.push(CheckViolation::PhantomMergeDriver {
                    repo: repo.clone(),
                    pattern,
                    driver,
                });
            }
        }
    }

    violations
}

/// Extract `(pattern, driver)` for every `.gitattributes` line assigning an
/// `rwv-`-prefixed merge driver rwv does not define.
///
/// At most one finding per line: git resolves repeated assignments of one
/// attribute on a single line last-wins, so the last `merge=` token is what
/// the line means. Across lines it does not resolve pattern overlap — each
/// line is a declaration the operator wrote, and a phantom on any of them is
/// a mistake worth naming even if a later line would win for some path.
fn phantom_merge_drivers_in(contents: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        // Blank, comment, or a macro definition (`[attr]name …`), which
        // declares an attribute rather than assigning one to a path.
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((pattern, attrs)) = split_attribute_line(line) else {
            continue;
        };
        let effective_driver = attrs
            .split_whitespace()
            .filter_map(|token| token.strip_prefix("merge="))
            .next_back();
        if let Some(driver) = effective_driver {
            if driver.starts_with(RWV_MERGE_DRIVER_PREFIX) && !rwv_defines_merge_driver(driver) {
                found.push((pattern.to_owned(), driver.to_owned()));
            }
        }
    }
    found
}

/// Split a `.gitattributes` line into its path pattern and the attribute
/// tokens that follow, honouring the double-quoted form gitattributes(5)
/// allows for patterns containing spaces (`"a path/*" merge=rwv-ours`).
///
/// Escapes inside a quoted pattern are not interpreted: the pattern is
/// reported back to the operator, not matched against paths, so the raw text
/// between the quotes is the useful thing to show.
fn split_attribute_line(line: &str) -> Option<(&str, &str)> {
    if let Some(rest) = line.strip_prefix('"') {
        let end = rest.find('"')?;
        Some((&rest[..end], rest[end + 1..].trim_start()))
    } else {
        let mut parts = line.splitn(2, char::is_whitespace);
        let pattern = parts.next()?;
        Some((pattern, parts.next().unwrap_or("").trim_start()))
    }
}

// ---------------------------------------------------------------------------
// Clone-topology scanning
// ---------------------------------------------------------------------------

/// Scan every `(workspace, repo)` pair under this weave's view and report
/// clone-topology violations of the I1/I2 invariants from
/// `docs/explanation/joints/clone-topology.md`.
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
pub fn scan_clone_topology(
    vcs: &dyn crate::vcs::Vcs,
    ws_root: &Path,
    repo_paths: &BTreeSet<RepoPath>,
) -> Vec<CheckViolation> {
    use crate::workweave::{classify_checkout, CheckoutKind};

    let mut violations = Vec::new();
    if repo_paths.is_empty() {
        return violations;
    }

    // Collect every workweave under this weave once; we iterate per-repo
    // inside the loop.
    let workweaves = crate::workweave::list_workweave_dirs(ws_root);

    for repo in repo_paths {
        let canonical_slot = ws_root.join(repo.as_path());
        let canonical_store_raw = vcs.resolve_canonical_store(&canonical_slot);

        // Expected canonical store path: `<canonical_slot>/.git`. Compare via
        // canonicalize to absorb any trailing-slash / symlink differences.
        let expected_store = crate::git::store_path_in(&canonical_slot);
        let expected_store_canon = expected_store
            .canonicalize()
            .unwrap_or_else(|_| expected_store.clone());

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

            let ww_store_raw = match vcs.resolve_canonical_store(&ww_checkout) {
                Some(p) => p,
                None => continue, // not a workspace there; skip silently
            };
            let ww_store_canon = ww_store_raw
                .canonicalize()
                .unwrap_or_else(|_| ww_store_raw.clone());
            let ww_self_store = crate::git::store_path_in(&ww_checkout);
            let ww_self_store_canon = ww_self_store
                .canonicalize()
                .unwrap_or_else(|_| ww_self_store.clone());

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
                        expected_store_path: expected_store.clone(),
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
// `head_attachment`, `list_local_branch_names`, `head_revision`,
// `resolve_revision`, and `is_ancestor` — without any git-specific code.
// See `docs/explanation/joints/vcs-as-seam.md`.

/// Whether `name` is worth **showing** the operator as a possible leftover of
/// the pre-flat naming scheme.
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
/// name, not through a guess.
fn looks_like_a_pre_flat_ref(name: &str) -> bool {
    match name.split_once('/') {
        Some((lhs, segment)) => {
            !segment.is_empty()
                && crate::naming::split_at_weave_separator(lhs).is_some_and(
                    |(project, workweave)| !project.is_empty() && !workweave.is_empty(),
                )
        }
        None => false,
    }
}

/// Whether a **recorded** name carries a `/` segment, i.e. whether the
/// registry is holding a receipt for a pre-flat name.
///
/// Deliberately not [`looks_like_a_pre_flat_ref`], which is the predicate
/// for *observed* names — it screens a whole branch listing, so it has to
/// insist on the `<a>--<b>/<c>` shape to keep an operator's `feature/x` out
/// of the report. Here the population is already narrow: every name in the
/// registry arrived through [`EphemeralRefName::mint`] or through
/// [`LegacyEphemeralRefName::claim`] under a minted name, and both spell the
/// workweave's ref flat. So `contains('/')` is the whole
/// question, and asking a shape question on top would only add ways for a
/// false record to slip past.
///
/// One caveat this does **not** decide alone: [`mint`] does not validate its
/// components — the legal grammar for a name is undecided — so a workweave
/// *named* `a/b`
/// mints `p--a/b` — a segmented name that is nonetheless a live workweave's
/// own ref. Every caller pairs this with the liveness question before
/// retracting anything; see [`scan_pre_flat_receipts`].
///
/// [`EphemeralRefName::mint`]: crate::vcs::EphemeralRefName::mint
/// [`LegacyEphemeralRefName::claim`]: crate::vcs::LegacyEphemeralRefName::claim
/// [`mint`]: crate::vcs::EphemeralRefName::mint
fn receipt_names_a_pre_flat_ref(name: &crate::vcs::RawRefName) -> bool {
    name.as_str().contains('/')
}

/// The repos of one workweave the migration pass visits, each paired with
/// the canonical store its receipts key to.
///
/// The enumeration covers every worktree-materialized repo (skipping
/// [`ReferenceAlias`] checkouts) **and the project-repo checkout**, which the
/// member walker does not reach — delete handles it as a separate arm for the
/// same reason, and an implementer who reuses the member walker alone leaks
/// one project-repo branch per workweave.
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
    out.push(project_dir(workweave_dir, project_name));
    out.retain(|abs| abs.is_dir() && classify_checkout(abs) != CheckoutKind::ReferenceAlias);
    out
}

/// The refs of this workweave's own namespace that exist in `store`,
/// **attached or not**.
///
/// The pass enumerates refs per store — attached and unattached — not
/// attachment states, because a pass keyed on `head_attachment` alone
/// silently disowns a commit-bearing legacy branch that a fetch left
/// behind.
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
    // VCS is asked to match on is a name.
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
/// answer the canonical-store pass splits on — whether the workweave that ref was
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
    /// cannot see, which stays open; that is why `None` alone never
    /// authorizes anything, only a warrant does).
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
    /// out, and the undecided grammar for project and workweave names makes
    /// it unsound anyway.
    ///
    /// [`EphemeralRefName::mint`]: crate::vcs::EphemeralRefName::mint
    live_ref_names: std::collections::HashMap<crate::vcs::RawRefName, String>,
}

/// The weave's ownership receipts, arranged for the
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
        // with no receipts, which is every weave until the migration runs.
        let mut workweave_dirs: Option<Vec<(String, PathBuf)>> = None;
        for name in crate::workspace::discover_projects(ws_root) {
            let registry = RefRegistry::for_project(ws_root, &name);
            // A legacy index reads as "no receipts", which is the
            // fail-closed direction: nothing in it is destroyable until
            // the migration adopts it.
            match registry.list_all() {
                Ok(all) if all.is_empty() => continue,
                Ok(_) => {}
                Err(_) => continue,
            }
            let dirs = workweave_dirs
                .get_or_insert_with(|| crate::workweave::list_workweave_dirs(ws_root));
            let mut live_ref_names = std::collections::HashMap::new();
            for workweave in live_workweave_names(ws_root, &name, dirs) {
                if let Ok(workweave_name) = crate::manifest::WorkweaveName::new(&workweave) {
                    let minted = EphemeralRefName::mint(&name, &workweave_name);
                    live_ref_names.insert(minted.to_raw(), workweave);
                }
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

    /// Whether some workweave of `project` that is still on disk mints
    /// `name`.
    ///
    /// The same question [`for_store`](Self::for_store) answers per receipt,
    /// asked without a store: a retraction pass has a project and a recorded
    /// name and wants to know whether a live workweave would claim it before
    /// dropping the record. A project with no receipts was dropped at
    /// construction and answers `false` — vacuously right, since it holds
    /// nothing to retract.
    fn mints_for_a_live_workweave(
        &self,
        project: &ProjectName,
        name: &crate::vcs::RawRefName,
    ) -> bool {
        self.projects
            .iter()
            .any(|p| &p.name == project && p.live_ref_names.contains_key(name))
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
///   placement outside them;
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
            if marker.project().as_str() == project.as_str() {
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
/// for the migration arms.
///
/// One pass, because it is one question asked of one place: what refs does
/// this workweave's namespace hold in each store, and what is each checkout
/// attached to. Both halves are needed — the pass enumerates refs per store,
/// attached and unattached, not attachment states — because a legacy branch
/// a fetch left behind is invisible to a HEAD read, and a checkout on `main`
/// is invisible to a ref listing.
///
/// The healthy state is the **minted** name, flat: `<project>--<workweave>`,
/// no segment. The arms, and the sub-kind each produces:
///
///   * on the minted ref — healthy, nothing reported.
///   * on a pre-flat ref of this workweave's own namespace —
///     [`UnmigratedEphemeralBranch`], which `--fix` renames.
///   * on a ref some project **recorded** for another workweave —
///     [`ForeignEphemeral`].
///   * on any other branch — [`SharedBranch`]; covers the
///     bare-main-in-workweave case.
///   * detached — [`Detached`], carrying the pre-flat branch and **both**
///     tips when one exists, because those are the two things the operator
///     is choosing between.
///   * unborn — [`UnbornCheckout`]. Report-only: there is no revision to
///     record a receipt against.
///   * independently of the attachment: the minted ref present with no
///     receipt — [`UnrecordedEphemeralBranch`], which `--fix` adopts.
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

    let Ok(project) = ProjectName::new(project_name) else {
        return;
    };
    let Ok(workweave) = crate::manifest::WorkweaveName::new(workweave_name) else {
        return;
    };
    let flat = EphemeralRefName::mint(&project, &workweave);
    let expected_ref = flat.to_string();

    for abs in workweave_checkouts(vcs, workweave_dir, project_name) {
        // The receipt, if any, lives in this checkout's canonical store —
        // resolved from the checkout itself rather than assembled from the
        // primary and a manifest path, so an inverted topology reports
        // against the store the refs are actually in. Resolved per repo
        // because a receipt is keyed by (store, name).
        let store = crate::workweave::receipt_store_for(vcs, &abs);
        let store_receipts = recorded.for_store(&store);
        let flat_raw = flat.to_raw();
        let recorded_ref = store_receipts
            .iter()
            .find(|rec| rec.owned.name() == &flat_raw)
            .map(|rec| rec.owned.to_string());

        let (flat_present, legacy_refs) = refs_in_workweave_namespace(vcs, &store, &flat);

        // Asked of the *store*, not of the attachment: the flat ref can
        // exist with no receipt while HEAD sits somewhere
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
                    // Which of the two the migration can act on is decided
                    // by the same count the migration pass skips on, so the
                    // report never promises a rename git will refuse.
                    Some(_) if legacy_refs.len() > 1 => {
                        BranchDisciplineKind::BlockedEphemeralNamespace {
                            expected_ref: expected_ref.clone(),
                            blocking_refs: legacy_refs.iter().map(|r| r.to_string()).collect(),
                        }
                    }
                    Some(legacy) => BranchDisciplineKind::UnmigratedEphemeralBranch {
                        actual_branch: legacy.to_string(),
                        expected_ref: expected_ref.clone(),
                    },
                    // Foreign vs shared is decided by the registry (R2): a ref some project recorded for a
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
            Ok(HeadAttachment::Detached(d)) => {
                // When two or more refs share the namespace the same guard
                // that blocks UnmigratedEphemeralBranch also blocks the
                // consented detached arm — fix_branch_model_migration skips
                // the whole repo before reaching HeadAttachment::Detached.
                // Emit BlockedDetachedNamespace instead so the report never
                // names a remedy the guard prevents from running.
                if legacy_refs.len() > 1 {
                    BranchDisciplineKind::BlockedDetachedNamespace {
                        expected_ref: expected_ref.clone(),
                        at_sha: d.at().as_str().to_string(),
                        blocking_refs: legacy_refs.iter().map(|r| r.to_string()).collect(),
                    }
                } else {
                    BranchDisciplineKind::Detached {
                        expected_ref: expected_ref.clone(),
                        recorded_ref: recorded_ref.clone(),
                        at_sha: d.at().as_str().to_string(),
                        legacy_branch: legacy_refs
                            .first()
                            .map(|legacy| legacy_ref_at_tip(vcs, &store, legacy, d.at())),
                    }
                }
            }
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
/// `head` would strand it. A stranded tip **must** be warned about.
///
/// Structural: the question is ancestry, never how long ago
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
/// The two flavours mirror the two publish gates in `push.rs`: a manifest
/// member's counterpart is the local projection of its declared `version:`,
/// the project repo's is the local projection of the remote's declared
/// default branch. The project repo *is* an instance of the branch model, so
/// it gets an arm here rather than an exemption.
enum TrackingSource {
    /// A manifest member with exactly one declared `version:` across the
    /// projects that reference it.
    Declared(crate::vcs::TrackingRef),
    /// The project repo. Its counterpart is observed, not declared: what a
    /// channel's publish ref is stays open, and reading the remote's own HEAD
    /// answers "which branch is this repo's trunk" without deciding it.
    RemoteDefault,
    /// No declaration resolves — the repo is on disk but in no manifest, or
    /// two projects declare different `version:` values for it. Nothing can
    /// be named as a reattach target, so the Detached arm reports only.
    Unresolvable,
}

/// One canonical store the canonical-store pass visits.
struct CanonicalStore {
    /// Absolute path of the store.
    path: PathBuf,
    tracking: TrackingSource,
}

impl CanonicalStore {
    /// The local branch a detached HEAD here would reattach to.
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
/// directories. `projects/<project>/` does **not** — the scan there is by
/// workspace, not by registry directory, so the project
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

    for project in crate::workspace::discover_projects(ws_root) {
        let path = project_dir(ws_root, project.as_str());
        // "Not a repo" is a typed error, not a state, so the enumeration
        // can ask the question directly instead of guessing from a
        // collapsed `None`.
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

/// Scan every canonical store under `ws_root` — manifest members and
/// `projects/<project>/`, which is an instance of the model like any other —
/// for the canonical-store arms plus (c) stale-ephemeral-branches.
///
/// The arms, in order:
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
///     is where the unborn HEAD is reported.
///   * `Detached(_)` — [`CanonicalDetached`].
///
/// (c) leaked ephemeral refs. **Ranges over the store's receipts**, not over
/// its branch listing: "this ref belongs to a workweave that is gone" is a
/// question about the record, and the branch listing cannot answer it without
/// taking a name apart — which R2 forbids, and which the flat-name cutover
/// removed the machinery for. A receipt whose ref still
/// exists and whose workweave no longer does is split two ways: safe (the tip
/// is an ancestor of the store's tip, so a [`Merged`] warrant can be
/// established) and live (the tip carries commits the store's tip does not).
///
/// "No longer does" is asked of three sources, because the two that are
/// rwv's own record both miss a seat placed outside every container whose
/// index entry was lost: the container walk, the workweave index, and
/// [`crate::vcs::Vcs::live_worktree_branches`], which is git's worktree
/// table and holds such a seat by absolute path.
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
    // inside a *live* workweave's namespace is the migration's business and
    // is already reported there, with a fix attached.
    let live_namespaces = live_minted_ref_names(ws_root);

    for store in canonical_stores(vcs, ws_root, projects) {
        let abs = &store.path;
        let store_receipts = recorded.for_store(abs);

        // The match is exhaustive over the three states `head_attachment`
        // is total on, which is what makes the Detached arm impossible to
        // leave out; a collapsed `Option` has no branch for it.
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
                        // Both halves: the counterpart must exist as a
                        // LOCAL branch, and its tip must
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

        // Every ref this store's live checkouts hold, from git's own
        // worktree table. Unreadable is treated as "cannot answer", which
        // skips (c) for this store — the same direction the arms above take
        // on a store they cannot read.
        let Ok(held_by_live_worktrees) = vcs.live_worktree_branches(abs) else {
            continue;
        };

        for rec in &store_receipts {
            // A receipt whose workweave is still on disk is not leaked at
            // all. The receipt is the authority when the container scan
            // disagrees — a `--dir` placement outside every container is
            // invisible to that scan, and treating its live ref as a
            // leak is the exact failure the receipt rule exists to
            // prevent.
            if rec.live_workweave.is_some() {
                continue;
            }
            // Liveness above is rwv's own record, and a `--dir` placement
            // whose index entry was lost is in none of it. The worktree
            // table is where such a seat is still visible — git records
            // every checkout by absolute path, and refuses to delete a
            // branch one of them is on.
            if held_by_live_worktrees
                .iter()
                .any(|held| held == rec.owned.name())
            {
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
            // The receipt may name a pre-flat ref, which
            // `scan_pre_flat_receipts` owns — same reason, and here the
            // stakes are the other way round. The guard above has already
            // established that no live workweave mints this name, so a
            // segmented one would fall straight through to the safe/live
            // split below and be reported as a leak whose ownership rwv can
            // prove. It cannot: the record is the defect. Retracting it is
            // the repair, and `--fix` has already run that arm by the time
            // this scan is reached.
            if receipt_names_a_pre_flat_ref(rec.owned.name()) {
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
        // rwv minted before the flat cutover, that no receipt names and
        // that no live workweave's namespace claims. Report-only, forever:
        // under R2 it is not rwv's, and whose it was cannot be
        // reconstructed.
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
            // A live workweave still claims this namespace, so the
            // migration can rename it and the (a) pass already said so.
            if live_namespaces
                .iter()
                .any(|flat| crate::vcs::LegacyEphemeralRefName::claim(flat, name).is_some())
            {
                continue;
            }
            // `live_namespaces` is built from the same two records the
            // receipt loop consults, so it has the same blind spot, and here
            // the report says outright that no workweave claims the branch
            // and invites the operator to remove it.
            if held_by_live_worktrees.iter().any(|held| held == name) {
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
    // project from the marker rather than from `discover_projects` matters:
    // a workweave whose `projects/<project>/` slot is missing is still a
    // workweave, and treating its live ref as an orphan is the direction
    // that turns a real branch into a "leftover".
    for (name, dir) in crate::workweave::list_workweave_dirs(ws_root) {
        if let Ok(Some(marker)) = crate::workspace::WorkweaveMarker::read(&dir) {
            if let Ok(workweave_name) = crate::manifest::WorkweaveName::new(&name) {
                out.push(EphemeralRefName::mint(marker.project(), &workweave_name));
            }
        }
    }
    // The indexes, which are the only record of a `--dir` placement outside
    // every container. Consulted only for entries whose directory
    // actually exists, so a stale entry cannot resurrect a deleted workweave.
    for project in crate::workspace::discover_projects(ws_root) {
        if let Ok(Some(index)) = crate::workweave_index::read(ws_root, &project) {
            for (name, path) in &index.workweaves {
                if path.is_dir() {
                    if let Ok(workweave_name) = crate::manifest::WorkweaveName::new(name) {
                        out.push(EphemeralRefName::mint(&project, &workweave_name));
                    }
                }
            }
        }
    }
    out
}

/// Scan every project's receipt registry for receipts whose ref is not in
/// the store they name — the benign residue of a crash between the receipt
/// write and the ref creation.
///
/// Only stores that are present and readable are considered. A receipt whose
/// store has gone is R4 territory — whether receipts are reclaimed in bulk
/// under a store-destroy is open — and retracting one here would answer that
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

    for project in crate::workspace::discover_projects(ws_root) {
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

/// Scan every project's receipt registry for receipts naming a pre-flat ref
/// — a recorded name carrying a `/` segment that no live workweave mints.
///
/// The sibling of [`scan_dangling_receipts`] and not a case of it: that one
/// asks the store whether the ref is there, and here it usually is. The
/// residue is the *record*. The pre-flat migration writes exactly this
/// receipt on its success path (adopt the pre-flat name, rename it flat,
/// retract) and
/// leaves it behind whenever the rename does not complete — the two-refs-in-
/// one-namespace skip being the shape that guarantees it never will until an
/// operator intervenes.
///
/// **Why it may be dropped.** The canonical-store pass asks which live
/// workweave mints a recorded name; none mints a segmented one, so the ref
/// reads as a leak, and a leak with a receipt is in the class `--fix`
/// deletes from. The record is what manufactures that warrant, and it
/// records something rwv cannot have created: every ref rwv mints is flat.
///
/// **The liveness guard is not belt-and-braces.**
/// [`EphemeralRefName::mint`] does not validate its components, so a
/// workweave literally named `a/b` mints `p--a/b`, and that receipt is a
/// live workweave's own — the one segmented name that is true. Retracting it
/// would disown a workweave nothing later re-adopts (the migration walks the
/// container scan, which cannot see such a placement at all), leaving a ref
/// no verb may ever clean up. So the name test alone is not the finding; it
/// only selects the candidates the liveness question is then asked about.
///
/// **The store is never consulted.** [`scan_dangling_receipts`] visits only
/// present, readable stores because its question is about a ref *in* one,
/// and a receipt whose store has gone is R4 territory it declines to
/// answer. The question here is about the record alone — a segmented name is
/// one rwv could not have minted whether or not the store is on disk — so
/// the same guard would only strand a false receipt behind a repo someone
/// removed. Nothing is answered about bulk reclamation either way.
///
/// Cheap on every weave that has none: the name test runs over the registry
/// first, and the container walk behind [`RecordedRefs`] is built only if
/// some candidate survives it.
///
/// `active_project` scopes the walk exactly as [`scan_dangling_receipts`]
/// does — a filter on which registries are opened.
///
/// [`EphemeralRefName::mint`]: crate::vcs::EphemeralRefName::mint
fn scan_pre_flat_receipts(
    ws_root: &Path,
    active_project: Option<&str>,
    out: &mut Vec<CheckViolation>,
) {
    use crate::workweave_index::RefRegistry;

    let mut candidates = Vec::new();
    for project in crate::workspace::discover_projects(ws_root) {
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
            if receipt_names_a_pre_flat_ref(owned.name()) {
                candidates.push((project.clone(), owned));
            }
        }
    }
    if candidates.is_empty() {
        return;
    }

    let recorded = RecordedRefs::new(ws_root);
    for (project, owned) in candidates {
        if recorded.mints_for_a_live_workweave(&project, owned.name()) {
            continue;
        }
        out.push(CheckViolation::PreFlatRefReceipt {
            project,
            store_path: owned.store().to_path_buf(),
            ref_name: owned.to_string(),
        });
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
        let Ok(workweave_name) = WorkweaveName::new(workweave_name_str) else {
            continue;
        };
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

/// Scan branch-discipline (workweave-branch + the canonical-store arms +
/// stale-ephemeral-branches) across the workspace
/// rooted at `ws_root` (which must be the primary).
///
/// One symbolic-ref read per workweave checkout plus one branch listing
/// per canonical store. The check is VCS-neutral: it consumes only the
/// [`Vcs`] trait surface and never spells git plumbing.
///
/// `projects` supplies the tracking declarations the Detached arm projects
/// a reattach target from; pass every loaded project. With an empty
/// slice the arm still reports, it just cannot name a counterpart.
///
/// See:
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
            marker.project().as_str(),
            &workweave_name,
            &mut violations,
        );
    }

    // (b) + (c) — the canonical-store pass over every store under the
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
///   [`crate::vcs::Vcs::list_stale_worktree_registrations`]. `--fix` (in `run_check`)
///   runs [`crate::vcs::Vcs::worktree_prune`]; this scanner only reports.
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
        // presence, not to debug its content.
        if let Ok(Some(state)) = crate::op_state::read_owner(&target.workspace_dir) {
            violations.push(CheckViolation::StaleOpState {
                workspace_dir: target.workspace_dir.clone(),
                verb: state.verb,
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
///   (`<ws_root>/../.workweaves/<ww_dir>/`).  The project is read from the
///   workweave directory's `.rwv-workweave` marker — the same source the
///   scan that produced the finding minted from, and the same source
///   `fix_branch_model_migration` scopes its repairs on. Reading the
///   directory basename here instead (the pre-fix behavior) made the report
///   and the repair scope the same finding to different projects whenever a
///   hand-renamed directory disagreed with its marker: the report showed a
///   remedy under one project while `--fix` skipped it under the other.
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
            // (a) path: under <container>/<ww_dir>/... — the project is the
            // marker's, matching the mint and the repair scope.
            let ww_dir_name = rel_from_ww_parent
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned());
            if let Some(dir_name) = ww_dir_name {
                let ww_dir = ww_parent.join(&dir_name);
                if let Ok(Some(marker)) = crate::workspace::WorkweaveMarker::read(&ww_dir) {
                    return marker.project().as_str() == active_project;
                }
            }
            // No readable marker → conservative: exclude. The tree-integrity
            // scan owns reporting the broken marker.
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
        // The project repo is not a manifest member, so it is not in
        // `known_repos`: `projects/<name>` is in scope exactly when `<name>`
        // is the active project. Without this arm every project-repo finding
        // would be filtered out of the default (project-scoped) run, which
        // is the scope hole this arm closes.
        if let Some(name) = strip_projects_prefix(Path::new(&rel_str)) {
            return name.to_string_lossy() == active_project;
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
/// state `scan_dangling_receipts` exists to clear. Retracting the receipt
/// of a ref this call just destroyed is bookkeeping, not reclamation policy
/// — whether receipts are reclaimed in bulk stays open.
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
                 ownership receipt for it — a ref that looks like rwv's is not rwv's",
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

/// Apply the migration pass — the flat-name cutover's other half.
///
/// Runs per workweave, and per repo checkout within it: members **and** the
/// project repo, which the member walker does not reach. The arms:
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
/// **No in-flight operation state.** An operator who upgrades while a sync
/// is stopped mid-rebase resolves or aborts it first, without being told to
/// migrate. A workweave with op state is skipped with a message naming
/// `rwv abort`; the rest of the weave still migrates.
///
/// **The flat name must be reachable.** At most one ref per (workweave,
/// store) can exist; git holds `refs/heads/p--w` and `refs/heads/p--w/x`
/// as a file and a directory of the same name, so where two or more refs
/// share a namespace no arm can produce the flat one. That pair is skipped
/// before any arm runs — a receipt written for a rename that then fails
/// claims a pre-flat name, which resolves to no workweave on disk and so
/// reads as stale and deletable. Collapsing the namespace is an
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

    for (workweave_name, workweave_dir) in crate::workweave::list_workweave_dirs(ws_root) {
        let Ok(Some(marker)) = crate::workspace::WorkweaveMarker::read(&workweave_dir) else {
            continue;
        };
        if let Some(active) = active_project {
            if marker.project().as_str() != active {
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

        let workweave_name_typed = match crate::manifest::WorkweaveName::new(&workweave_name) {
            Ok(n) => n,
            Err(e) => {
                errors.push(format!(
                    "{}: skipped the branch-model migration — {e}",
                    workweave_dir.display()
                ));
                continue;
            }
        };
        let flat = EphemeralRefName::mint(marker.project(), &workweave_name_typed);
        let mut registry = RefRegistry::for_project(ws_root, marker.project());

        for abs in workweave_checkouts(vcs, &workweave_dir, marker.project().as_str()) {
            let store = crate::workweave::receipt_store_for(vcs, &abs);
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
            // BEFORE it writes the ref, so acting here
            // persists an ownership claim for a rename that did not happen —
            // and a receipt for a pre-flat name is worse than no receipt at
            // all: the owning workweave is derived from the ref name, and
            // under flat naming a name with a segment resolves to no
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

    (applied, errors)
}

/// Adopt a pre-flat ref into a receipt, then rename it flat.
///
/// A rename is a DESTROY of the old name plus a birth of the new, so the DESTROY takes the old name's receipt and a warrant, and the
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
/// MIGRATORY arm: renames pre-flat refs minted by rwv <= v0.15.0.
/// Removable once every owned weave's health floor records a clean doctor
/// at >= v0.18 after a migration-complete run (see
/// [`crate::health_floor`]).
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
    //
    // `fix_pre_flat_receipts` now clears the same receipt earlier in a
    // `--fix` run, so on that path this rarely finds anything. It stays
    // because the pass is callable on its own and its idempotence over its
    // own crash residue is its property, not something to borrow from
    // doctor's arm ordering.
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

/// Record a receipt for a flat ref that exists without one.
///
/// The tip is read here and recorded as `created_at`, which is what the doc
/// specifies ("adopt it: write a receipt at the observed tip") — and what
/// makes the pass idempotent over its own partial output, because a re-run
/// finds the receipt already there and `record_created` does nothing.
///
/// MIGRATORY arm: adopts flat refs minted without receipts during the
/// v0.16.0 development window only. Removable on the same floor as the
/// pre-flat rename arm (see [`crate::health_floor`]).
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

/// Mint the workweave's flat ref at a detached HEAD.
///
/// There may or may not be a pre-flat ref. When there is, git cannot hold
/// both `refs/heads/p--w` and `refs/heads/p--w/<segment>`, so the flat name
/// can only exist once the pre-flat one stops — which is precisely the
/// stranding the caller must warn about, and why the operator's consent is
/// required even when nothing is lost.
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

/// Apply the `rwv doctor --fix --reattach-checkouts` reattach for a
/// detached canonical store.
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
/// lock SHA — which is most detached repos in most weaves. This
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
        // Not "the counterpart exists" alone: reattaching
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

/// Apply the `rwv doctor --fix` retraction for dangling ownership
/// receipts.
///
/// Safe by construction: a receipt naming a ref that does not exist
/// authorizes nothing — no warrant can be built against an absent ref — so
/// dropping it destroys no capability and no work.
///
/// The absence check lives in `scan_dangling_receipts` and nowhere else,
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

/// Apply the `rwv doctor --fix` retraction for ownership receipts naming a
/// pre-flat ref — the residue `scan_pre_flat_receipts` classifies.
///
/// Safe by construction, and for a different reason than
/// [`fix_dangling_receipts`]: there the receipt authorizes nothing because
/// the ref is absent; here the ref is present and the receipt authorizes
/// rather too much. Retraction is the only operation involved. It writes to
/// the registry and never to the store, so no ref moves and no commit is
/// reachable from one place fewer afterwards. What the weave is left with is
/// an Unowned ref: reported, and under R2 not rwv's to delete.
///
/// **Ordering — this runs *before* [`fix_branch_model_migration`], and that
/// is the design.** The pre-flat migration holds exactly this receipt for
/// the width of its rename, so an arm that ran after the migration would be reading state
/// the migration had just written, and its safety would rest on
/// `migrate_legacy_ref` having retracted every receipt it took — a property
/// of a function three call levels away, silently load-bearing here.
/// Running first, this pass can only ever see what was on disk before doctor
/// touched anything: residue from earlier runs, never a receipt in flight.
/// The migration then re-adopts, at the tip it observes now, whatever is
/// still migratable — so one `--fix` both clears the false record and
/// finishes the migration where it can.
///
/// The classification lives in the scan and nowhere else, the way
/// [`fix_dangling_receipts`] keeps the absence check in
/// `scan_dangling_receipts`: a second copy of the liveness guard here
/// would be a safety property no fixture could open a window against, and an
/// unreachable guard is one that silently stops holding.
///
/// Returns `(retracted, errors)`: the `(store, ref name)` pairs disowned,
/// and per-receipt failures for the caller to surface as issues.
///
/// MIGRATORY arm: retracts receipts left by pre-v0.16.0 migration crashes.
/// Removable on the same floor as the pre-flat rename arm (see
/// [`crate::health_floor`]).
pub fn fix_pre_flat_receipts(
    ws_root: &Path,
    active_project: Option<&str>,
) -> (Vec<(PathBuf, String)>, Vec<String>) {
    use crate::workweave_index::RefRegistry;

    let mut retracted = Vec::new();
    let mut errors = Vec::new();

    let mut violations = Vec::new();
    scan_pre_flat_receipts(ws_root, active_project, &mut violations);
    for violation in violations {
        let CheckViolation::PreFlatRefReceipt {
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
                "failed to retract the ownership receipt for `{}` in {}: {e}",
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
///   [`crate::vcs::Vcs::worktree_prune`] in the registering repo. Information-
///   preserving by construction (the only state being dropped is a
///   pointer to a directory that already does not exist).
/// - [`CheckViolation::OrphanedSavepoint`] with
///   [`OrphanedSavepointKind::Redundant`] → [`crate::vcs::Vcs::drop_savepoint`].
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
/// committed schema artifact in the main branch, regenerated by
/// `cargo run --bin generate-explain`; CI fails on drift.
pub const DOCTOR_SCHEMA_URL: &str = crate::schema_url::schema_url!("doctor");

/// Output envelope for `rwv doctor --json`. By default only the active project
/// is checked and orphan detection is skipped; pass `--all` to scan every
/// project and enable weave-wide orphan detection. Findings arrive on two
/// disjoint arrays — `violations` for what rwv's own scans found, `issues` for
/// what an integration reported — and both empty means the checked scope is
/// clean. The `plugins` array is the PATH inventory of `rwv-*` executables
/// (reporting only — plugin presence never fails the doctor check or affects
/// the exit code).
#[derive(Debug, Serialize, JsonSchema)]
pub struct DoctorJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub violations: Vec<ViolationOutput>,
    /// Findings raised by an integration rather than by one of rwv's own
    /// scans: a missing ecosystem tool, drift or user-held content in a
    /// managed file, a surfacing symlink that does not resolve, a member
    /// incompatibility. Disjoint from `violations` — nothing on this array
    /// carries `kind: "core-finding"`.
    pub issues: Vec<IssueOutput>,
    /// Standing advisories this checkout raises, in the vocabulary
    /// `rwv sync --json` already emits: a condition with a named remedy and the
    /// paths that raised it. Empty, not absent, so a consumer branches on
    /// length.
    pub advisories: Vec<crate::workspace::AdvisoryOutput>,
    /// `rwv-*` executables discovered on `PATH`. Each record carries the verb
    /// name, absolute path, and a `shadowed` flag for duplicates: when the
    /// same name appears in multiple `PATH` directories, the first copy wins
    /// at exec time; later copies are marked `shadowed: true` with
    /// `shadowed_by` pointing at the winning binary. Records are sorted by
    /// `(name, path)` for deterministic output. An empty array means no
    /// `rwv-*` executables were found. Never a failed check — the inventory
    /// is the audit surface for the PATH trust boundary.
    pub plugins: Vec<crate::plugins::PluginRecord>,
    /// Resolved workspace coordinates (workspace root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// Inputs for running workspace-wide checks.
pub struct CheckInput {
    /// All repos referenced by any project's `rwv.toml`.
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

        // Coverage: every manifest repo should have a lock entry. Distinct
        // from the freshness comparison above — a repo absent from the lock is
        // invisible to it — and answered against the raw lock. Two states make
        // `resolve_versions` drop an entry: its repo is absent from disk, or
        // its revision will not resolve in this clone. Against the resolved
        // lock both read as no entry at all, and earn a finding whose remedy
        // is to write a line `rwv.lock` already carries.
        if let Some(lock) = &project.lock {
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

/// A finding class the text report collapses to one per-class count line.
///
/// The classes classified RECLAMATION (reclaims dead state; auto under
/// `--fix`) and the frozen-legacy classes (a backlog with no current
/// generator) render as ONE text line each — count, class, remedy — with
/// per-item detail in `--json` only. Doctor's gate function is the point:
/// the text report must read as distinct facts, and a hundred lines of the
/// same frozen backlog is one fact. The `--json` surface is untouched — the
/// per-class count baselines are captured from `violations[]` with jq, and
/// that instrument depends on the full records.
///
/// **The counts are the re-trigger.** A class's count regrowing past its
/// recorded post-sweep baseline is the structural signal that reopens the
/// question of a dedicated reclamation verb — a count against a recorded
/// floor, no wall-clock.
///
/// Membership is deliberate and closed:
///
/// * RECLAMATION — `stale-registry-entry`, `stale-worktree-registration`,
///   `stale-ephemeral-branch-safe`, `dead-op-lease`, `dangling-ref-receipt`,
///   redundant orphaned savepoints.
/// * Frozen legacy — live orphaned savepoints (teardown-leak backlog; the
///   leak is fixed and the newest orphan predates the fix),
///   `stale-ephemeral-branch-live` / `-unowned` (only legacy/refused ones
///   linger — deletion reaps the branch on the current path), and
///   `shared-branch` (the pre-scheme workweave backlog; the count line
///   keeps a fresh instance visible as regrowth).
///
/// Everything else stays itemized: a finding outside these classes is a
/// distinct fact the operator has not seen before, and collapsing it would
/// bury it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CollapsedClass {
    StaleRegistryEntry,
    StaleWorktreeRegistration,
    StaleEphemeralBranchSafe,
    StaleEphemeralBranchLive,
    StaleEphemeralBranchUnowned,
    DeadOpLease,
    DanglingRefReceipt,
    RedundantSavepoint,
    LiveSavepoint,
    SharedBranch,
}

impl CollapsedClass {
    /// The class `v` collapses into, or `None` for a finding that stays
    /// itemized.
    fn of(v: &CheckViolation) -> Option<Self> {
        match v {
            CheckViolation::WorkweaveTreeIntegrity {
                sub_kind: WorkweaveTreeIntegrityKind::StaleRegistryEntry { .. },
                ..
            } => Some(Self::StaleRegistryEntry),
            CheckViolation::StaleWorktreeRegistration { .. } => {
                Some(Self::StaleWorktreeRegistration)
            }
            CheckViolation::BranchDiscipline { sub_kind, .. } => match sub_kind {
                BranchDisciplineKind::StaleEphemeralBranchSafe { .. } => {
                    Some(Self::StaleEphemeralBranchSafe)
                }
                BranchDisciplineKind::StaleEphemeralBranchLive { .. } => {
                    Some(Self::StaleEphemeralBranchLive)
                }
                BranchDisciplineKind::StaleEphemeralBranchUnowned { .. } => {
                    Some(Self::StaleEphemeralBranchUnowned)
                }
                BranchDisciplineKind::SharedBranch { .. } => Some(Self::SharedBranch),
                _ => None,
            },
            CheckViolation::DeadOpLease { .. } => Some(Self::DeadOpLease),
            CheckViolation::DanglingRefReceipt { .. } => Some(Self::DanglingRefReceipt),
            CheckViolation::OrphanedSavepoint { sub_kind, .. } => match sub_kind {
                OrphanedSavepointKind::Redundant => Some(Self::RedundantSavepoint),
                OrphanedSavepointKind::Live => Some(Self::LiveSavepoint),
            },
            _ => None,
        }
    }

    /// Whether `rwv doctor --fix` repairs every member of the class.
    fn auto_fixed(self) -> bool {
        match self {
            Self::StaleRegistryEntry
            | Self::StaleWorktreeRegistration
            | Self::StaleEphemeralBranchSafe
            | Self::DeadOpLease
            | Self::DanglingRefReceipt
            | Self::RedundantSavepoint => true,
            Self::StaleEphemeralBranchLive
            | Self::StaleEphemeralBranchUnowned
            | Self::LiveSavepoint
            | Self::SharedBranch => false,
        }
    }

    /// The one text line the class renders: count, class, remedy, and where
    /// the per-item records are.
    fn issue(self, n: usize) -> Issue {
        let s = if n == 1 { "" } else { "s" };
        let body = match self {
            Self::StaleRegistryEntry => format!(
                "{n} stale-registry-entry finding{s} — registered workweave \
                 path{s} that no longer round-trip{verb}; `rwv doctor --fix` \
                 prunes them",
                verb = if n == 1 { "s" } else { "" },
            ),
            Self::StaleWorktreeRegistration => format!(
                "{n} stale-worktree-registration finding{s} — worktree \
                 registration{s} pointing at missing directories; \
                 `rwv doctor --fix` prunes them"
            ),
            Self::StaleEphemeralBranchSafe => format!(
                "{n} stale-ephemeral-branch-safe finding{s} — receipted \
                 branch{es} of deleted workweaves carrying no unique commits; \
                 `rwv doctor --fix` deletes them under warrant",
                es = if n == 1 { "" } else { "es" },
            ),
            Self::StaleEphemeralBranchLive => format!(
                "{n} stale-ephemeral-branch-live finding{s} — receipted \
                 branch{es} of deleted workweaves carrying unique commits; \
                 never auto-deleted, review and reclaim by hand",
                es = if n == 1 { "" } else { "es" },
            ),
            Self::StaleEphemeralBranchUnowned => format!(
                "{n} stale-ephemeral-branch-unowned finding{s} — branch{es} \
                 with no ownership receipt; not rwv's to delete",
                es = if n == 1 { "" } else { "es" },
            ),
            Self::DeadOpLease => format!(
                "{n} dead-op-lease finding{s} — lease file{s} whose recorded \
                 owner holds no matching op; `rwv doctor --fix` clears them"
            ),
            Self::DanglingRefReceipt => format!(
                "{n} dangling-ref-receipt finding{s} — receipt{s} naming refs \
                 that are not there (benign crash residue); `rwv doctor --fix` \
                 retracts them"
            ),
            Self::RedundantSavepoint => format!(
                "{n} redundant orphaned-savepoint finding{s} — savepoint tip{s} \
                 already anchored by a live branch; `rwv doctor --fix` drops them"
            ),
            Self::LiveSavepoint => format!(
                "{n} live orphaned-savepoint finding{s} — savepoint{s} holding \
                 commits no live ref anchors; report-only, reclaimed by the \
                 reviewed operator sweep (report-before-drop)"
            ),
            Self::SharedBranch => format!(
                "{n} shared-branch finding{s} — checkout{s} standing on a \
                 shared (non-ephemeral) branch; the switch target for each is \
                 in its record"
            ),
        };
        let message = format!("{body}; per-item detail: `rwv doctor --json`");
        Issue {
            kind: IssueKind::CoreFinding,
            integration: "core".into(),
            severity: crate::integration::Severity::Warning,
            message,
            safe_to_fix: self.auto_fixed(),
        }
    }
}

/// Convert check violations into the same `Issue` type that integrations use,
/// so all check results have a uniform shape.
///
/// Reclamation and frozen-legacy classes collapse to one count line each
/// (`CollapsedClass`); everything else renders itemized. The collapse is a
/// TEXT-report shape only — `--json` is built straight from
/// `CheckViolation` and carries every record.
pub fn violations_to_issues(violations: Vec<CheckViolation>) -> Vec<Issue> {
    let mut counts: std::collections::BTreeMap<CollapsedClass, usize> =
        std::collections::BTreeMap::new();
    let mut itemized = Vec::new();
    for v in violations {
        match CollapsedClass::of(&v) {
            Some(class) => *counts.entry(class).or_default() += 1,
            None => itemized.push(v),
        }
    }
    let mut issues = itemized_violations_to_issues(itemized);
    issues.extend(counts.into_iter().map(|(class, n)| class.issue(n)));
    issues
}

/// The per-item rendering behind [`violations_to_issues`] — every violation
/// here produces its own line (or is dropped by the one deliberate
/// json-only carve-out below).
fn itemized_violations_to_issues(violations: Vec<CheckViolation>) -> Vec<Issue> {
    violations
        .into_iter()
        .filter_map(|v| {
            // A foreign-primary marker that resolves to a different, valid
            // workspace is expected under a shared workweave container and
            // not this workspace's problem; every sibling weave's doctor
            // would otherwise repeat the same finding about every other
            // sibling. `--json` still carries it: `ViolationOutput` is built
            // straight from `CheckViolation`, not through this function.
            if matches!(
                &v,
                CheckViolation::WorkweaveTreeIntegrity {
                    sub_kind: WorkweaveTreeIntegrityKind::ForeignPrimaryOtherWorkspace { .. },
                    ..
                }
            ) {
                return None;
            }
            // safe_to_fix defaults to true; live-class branch-discipline
            // findings override to false so `doctor --fix` leaves them alone.
            let mut safe_to_fix = true;
            let (severity, message) = match v {
                CheckViolation::ConfusableSiblings {
                    parent,
                    first,
                    second,
                } => (
                    crate::integration::Severity::Warning,
                    crate::workspace::confusable_warning(
                        &crate::workspace::ConfusableSiblings {
                            parent: parent.clone(),
                            first: first.clone(),
                            second: second.clone(),
                        },
                    ),
                ),
                CheckViolation::OrphanedClone { path } => (
                    crate::integration::Severity::Error,
                    format!(
                        "orphaned clone: {path} — not listed in any project's rwv.toml; \
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
                         listed in rwv.toml but not cloned on disk; \
                         run `rwv fetch` from the workspace to re-materialize \
                         missing manifest members, then re-run `rwv doctor` to verify"
                    ),
                ),
                CheckViolation::MissingRole { project, repo } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "missing role in {project}: {repo} — \
                         add a `role: owned|dependency|reference` field to the \
                         rwv.toml entry for this repo"
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
                             recreate, or remove the repo from rwv.toml",
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
                CheckViolation::MissingReplayExclusion { project, sub_kind } => (
                    crate::integration::Severity::Warning,
                    match sub_kind {
                        ReplayExclusionKind::Absent => format!(
                            "{project}: project repo missing `rwv.lock merge=rwv-ours` in .gitattributes \
                             (run `rwv doctor --fix` to add)"
                        ),
                        ReplayExclusionKind::LegacySpelling => format!(
                            "{project}: project repo has legacy `rwv.lock merge=ours` in .gitattributes; \
                             the driver was renamed to close a global-config collision hazard \
                             (run `rwv doctor --fix` to migrate to `rwv.lock merge=rwv-ours` \
                             and commit)"
                        ),
                        ReplayExclusionKind::LegacyAlongsideCurrent => format!(
                            "{project}: project repo has both `rwv.lock merge=rwv-ours` and legacy \
                             `rwv.lock merge=ours` in .gitattributes; git picks between them by \
                             reading order, and the legacy name binds to any `merge.ours.driver` \
                             a global git config defines (run `rwv doctor --fix` to drop the \
                             legacy line and commit)"
                        ),
                    },
                ),
                CheckViolation::ReplayExclusionUnreadable { project, error } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{project}: failed to read .gitattributes for replay-exclusion check: {error}"
                    ),
                ),
                CheckViolation::MissingMergeDriverConfig {
                    project,
                    config_key,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{project}: project repo missing `{config_key}` config \
                         (defines the `rwv-ours` merge driver used by bare \
                         `git rebase --continue`; run `rwv doctor --fix` to plant)"
                    ),
                ),
                CheckViolation::MergeDriverConfigUnreadable {
                    project,
                    config_key,
                    error,
                } => (
                    crate::integration::Severity::Warning,
                    format!("{project}: failed to read `{config_key}` config: {error}"),
                ),
                CheckViolation::HeadUnreadable { repo, error } => (
                    crate::integration::Severity::Error,
                    format!("{repo}: HEAD unreadable ({error})"),
                ),
                CheckViolation::ProjectsDirUnreadable { path, error } => (
                    crate::integration::Severity::Error,
                    format!(
                        "{}: projects directory unreadable ({error}); every project under it \
                         is invisible to this scan",
                        path.display()
                    ),
                ),
                CheckViolation::ProjectlessDir { dir } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{}: directory under projects/ holds no {} at any depth, so it is \
                         not a project and rwv lists nothing for it; write a {} there \
                         (`[repositories]` alone is enough) or remove the directory",
                        dir.display(),
                        Manifest::FILE_NAME,
                        Manifest::FILE_NAME,
                    ),
                ),
                CheckViolation::UnnameableProject {
                    dir,
                    derived,
                    error,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{}: holds a {} but `{derived}` is not a usable project name \
                         ({error}), so no verb can address it; rename the directory to a \
                         name rwv accepts",
                        dir.display(),
                        Manifest::FILE_NAME,
                    ),
                ),
                CheckViolation::UnresolvableLockEntry { project, repo } => (
                    crate::integration::Severity::Error,
                    format!(
                        "{project}: lock references unknown revision for {repo}; \
                         run `rwv lock` or fetch"
                    ),
                ),
                CheckViolation::LegacyManifestFormat {
                    project,
                    legacy_path,
                } => (
                    crate::integration::Severity::Error,
                    format!(
                        "{project}: {}",
                        Manifest::legacy_format_refusal(legacy_path.as_path())
                    ),
                ),
                CheckViolation::LegacyWorkweaveMarker { marker_path, .. } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{} is a legacy (YAML) workweave marker; \
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
                         `rwv doctor --fix` to add the field — it is the precondition \
                         for every other arm of the migration",
                        index_path.display()
                    ),
                ),
                CheckViolation::UnreadableOwnedState {
                    project,
                    state_path,
                    error,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{}: rwv's record of the generations it accepted does not \
                         parse ({error}); until it is rebuilt, the managed-file \
                         drift and derived-state staleness checks report nothing \
                         for project `{project}` — including for files that had \
                         already drifted. Run `rwv materialize` to re-derive its \
                         generated files and record them afresh",
                        state_path.display()
                    ),
                ),
                CheckViolation::UnreadableWorkweaveIndex {
                    project,
                    index_path,
                    error,
                } => (
                    crate::integration::Severity::Error,
                    format!(
                        "{}: workweave index does not parse ({error}); the recorded \
                         workweaves and ownership receipts for project `{project}` are \
                         unevaluated, and every `rwv doctor --fix` arm that writes this \
                         file fails until it parses — repair or delete the file, then \
                         re-run `rwv doctor --fix` to re-adopt the workweaves on disk",
                        index_path.display()
                    ),
                ),
                // Relayed, not narrated: the loader states which file failed
                // and what to do about it, and this arm cannot know either.
                CheckViolation::UnparseableProject {
                    project, message, ..
                } => (
                    crate::integration::Severity::Error,
                    format!("{project}: {message}"),
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
                CheckViolation::WeaveRootIdentityConflict {
                    root,
                    pointer_project,
                    sub_kind,
                } => {
                    let pointer = match &pointer_project {
                        Some(p) => format!("names `{p}`"),
                        None => "is empty".to_string(),
                    };
                    let msg = match &sub_kind {
                        WeaveRootIdentityConflictKind::RegisteredWorkweave {
                            project,
                            workweave_name,
                        } => format!(
                            "{}: carries both `.rwv-active` (which {pointer}) and \
                             `.rwv-workweave`; the two are mutually exclusive. This directory \
                             is recorded as workweave `{workweave_name}` of project \
                             `{project}`, so the marker is authoritative and the pointer is \
                             an unread duplicate; run `rwv doctor --fix` to delete \
                             `.rwv-active` here (the marker is left alone)",
                            crate::path_spelling::operator_path(&root)
                        ),
                        WeaveRootIdentityConflictKind::MarkerUnverifiable {
                            marker_path,
                            defect,
                        } => format!(
                            "{}: carries both `.rwv-active` (which {pointer}) and \
                             `.rwv-workweave`; the two are mutually exclusive. {} \
                             `--fix` does not touch the pointer until the marker is repaired",
                            crate::path_spelling::operator_path(&root),
                            defect.refusal(marker_path)
                        ),
                        WeaveRootIdentityConflictKind::Unwitnessed { detail } => format!(
                            "{}: carries both `.rwv-active` (which {pointer}) and \
                             `.rwv-workweave`; the two are mutually exclusive. {detail} \
                             Nothing outside this directory says which file is the stray, so \
                             `--fix` does not touch either — delete the one you know to be \
                             wrong by hand",
                            crate::path_spelling::operator_path(&root)
                        ),
                    };
                    (crate::integration::Severity::Error, msg)
                }
                CheckViolation::WorkweaveTreeIntegrity {
                    workweave_dir,
                    sub_kind,
                } => {
                    let msg = match &sub_kind {
                        WorkweaveTreeIntegrityKind::DanglingParent { parent_path } => format!(
                            "{}: marker `parent` points to `{}` which does not exist; \
                             run `rwv doctor --fix` to re-point parent to primary",
                            crate::path_spelling::operator_path(&workweave_dir),
                            crate::path_spelling::operator_path(parent_path)
                        ),
                        WorkweaveTreeIntegrityKind::ParentChainAnomaly { detail } => format!(
                            "{}: workweave parent-chain anomaly: {}",
                            crate::path_spelling::operator_path(&workweave_dir),
                            detail
                        ),
                        WorkweaveTreeIntegrityKind::UnregisteredDir => format!(
                            "{}: directory under workweaves parent has no `.rwv-workweave` marker",
                            crate::path_spelling::operator_path(&workweave_dir)
                        ),
                        WorkweaveTreeIntegrityKind::ForeignPrimary { marker_primary } => format!(
                            "{}: marker `primary` (`{}`) does not match this workspace; \
                             this workweave may have been copied from another machine",
                            crate::path_spelling::operator_path(&workweave_dir),
                            crate::path_spelling::operator_path(marker_primary)
                        ),
                        WorkweaveTreeIntegrityKind::ForeignPrimaryOtherWorkspace {
                            marker_primary,
                        } => format!(
                            "{}: belongs to workspace `{}`; not this workspace's to manage",
                            crate::path_spelling::operator_path(&workweave_dir),
                            crate::path_spelling::operator_path(marker_primary)
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
                            crate::path_spelling::operator_path(recorded_path)
                        ),
                        WorkweaveTreeIntegrityKind::UnregisteredWorkweave {
                            project,
                            workweave_name,
                        } => format!(
                            "{}: workweave `{}` for project `{}` is present on \
                             disk but not recorded in `.rwv-workweave-index`; \
                             run `rwv doctor --fix` to adopt it",
                            crate::path_spelling::operator_path(&workweave_dir),
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
                            crate::path_spelling::operator_path(index_path),
                            project,
                            crate::path_spelling::operator_path(index_path)
                        ),
                        WorkweaveTreeIntegrityKind::UnreadableMarker { detail } => detail.clone(),
                        WorkweaveTreeIntegrityKind::NestedWorkweaveDir {
                            project,
                            workweave_name,
                            expected_dir_name,
                        } => format!(
                            "{}: workweave `{workweave_name}` for project `{project}` sits \
                             below its container instead of in it, from the era when a \
                             multi-segment project name rendered its `/` into the directory \
                             name; rwv now spells this workweave `{expected_dir_name}`. \
                             Retire this workweave and create it again — rwv does not rename \
                             a live workweave into place, and moving it by hand strands the \
                             worktrees inside it",
                            crate::path_spelling::operator_path(&workweave_dir),
                        ),
                        WorkweaveTreeIntegrityKind::MisnamedDir {
                            expected_dir_name,
                            detail,
                        } => match expected_dir_name {
                            Some(expected) => format!(
                                "{}: workweave directory name disagrees with its records — \
                                 {detail}. Rename the directory to `{expected}` to restore \
                                 the recorded identity",
                                crate::path_spelling::operator_path(&workweave_dir),
                            ),
                            None => format!(
                                "{}: workweave directory name disagrees with its records — \
                                 {detail}. Rename it to the `<project>--<name>` you intend, \
                                 or remove the directory",
                                crate::path_spelling::operator_path(&workweave_dir),
                            ),
                        },
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
                            // Both tips, side by side, and the two
                            // remediations in order: reattach first, adopt
                            // second.
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
                        // The fully automatic migration case.
                        BranchDisciplineKind::UnmigratedEphemeralBranch {
                            actual_branch,
                            expected_ref,
                        } => format!(
                            "{}: workweave checkout is on `{}`, the pre-flat \
                             `<project>--<workweave>/<segment>` shape rwv no longer \
                             mints; `rwv doctor --fix` records an ownership receipt \
                             for it and renames it to `{}` — a rename preserves the \
                             tip, so no commit moves",
                            repo_path.display(),
                            actual_branch,
                            expected_ref,
                        ),
                        BranchDisciplineKind::BlockedEphemeralNamespace {
                            expected_ref,
                            blocking_refs,
                        } => {
                            safe_to_fix = false;
                            format!(
                                "{}: {} refs share workweave namespace `{}` ({}), and git \
                                 cannot create the ref `{}` while any ref exists under \
                                 `{}/`. The branch-model migration skips this pair rather \
                                 than recording an ownership receipt for a rename that \
                                 cannot happen, so `rwv doctor --fix` will not touch it. \
                                 Which of those refs is this workweave's branch, and where \
                                 the others belong, is not rwv's call to make — leave at \
                                 most one ref under `{}/`, then re-run `rwv doctor --fix` \
                                 to migrate it",
                                repo_path.display(),
                                blocking_refs.len(),
                                expected_ref,
                                blocking_refs
                                    .iter()
                                    .map(|r| format!("`{r}`"))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                expected_ref,
                                expected_ref,
                                expected_ref,
                            )
                        }
                        BranchDisciplineKind::BlockedDetachedNamespace {
                            expected_ref,
                            at_sha,
                            blocking_refs,
                        } => {
                            safe_to_fix = false;
                            format!(
                                "{}: workweave checkout is detached at {} while {} refs \
                                 share its namespace `{}` ({}). git cannot create `{}` \
                                 while any ref exists under `{}/`, so \
                                 `--adopt-detached-checkouts` cannot run. Leave at most \
                                 one ref under `{}/`, then re-run `rwv doctor` to get the \
                                 ordinary detached-HEAD finding with a remedy that will \
                                 actually run",
                                repo_path.display(),
                                at_sha,
                                blocking_refs.len(),
                                expected_ref,
                                blocking_refs
                                    .iter()
                                    .map(|r| format!("`{r}`"))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                expected_ref,
                                expected_ref,
                                expected_ref,
                            )
                        }
                        BranchDisciplineKind::UnrecordedEphemeralBranch { branch } => format!(
                            "{}: branch `{}` is this workweave's ephemeral ref but rwv \
                             holds no ownership receipt for it. rwv deletes a branch \
                             only against a receipt it recorded, never on the strength \
                             of the name, so this one is not rwv's to delete and \
                             `rwv workweave delete` will leave it behind; \
                             `rwv doctor --fix` adopts it at its current tip",
                            repo_path.display(),
                            branch,
                        ),
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
                                 receipt for it. Ownership is by record, never by name \
                                 shape, so this branch is not rwv's to delete — and rwv \
                                 does not guess which workweave a stray ref belonged to. \
                                 `--fix` will never touch it; remove it by hand if it \
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
                         write and the ref creation. rwv writes the receipt first on \
                         purpose, so a crash leaves a receipt with no ref rather than a \
                         ref rwv could never destroy. It authorizes nothing; run \
                         `rwv doctor --fix` to retract it",
                        store_path.display(),
                        project,
                        ref_name
                    ),
                ),
                CheckViolation::PreFlatRefReceipt {
                    project,
                    store_path,
                    ref_name,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{}: project `{}` holds an ownership receipt for `{}`, whose name \
                         carries a `/` segment — no workweave on disk mints that name, so \
                         rwv cannot have created the ref under the flat scheme. Left \
                         recorded, the canonical-store scan reads the branch as a leak \
                         rwv owns and may delete. Run `rwv doctor --fix` to \
                         retract the receipt: that drops the record only — the branch is \
                         not touched, and afterwards it is unowned, which `--fix` never \
                         deletes",
                        store_path.display(),
                        project,
                        ref_name
                    ),
                ),
                CheckViolation::StaleOpState {
                    workspace_dir,
                    verb,
                    started_at,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{}/.rwv-op: stale-op-state present (started_at={started_at}); \
                         resume with `{resume}` or roll back with `rwv abort`. \
                         Never auto-fixed — another terminal may be mid-conflict-resolution.",
                        workspace_dir.display(),
                        resume = crate::op_state::resume_command(verb),
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
                    // Report-not-mandate: skew is informational.
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
                    // mismatch diagnostic actively misleads (blames
                    // crates.io). This finding is what agents/scripts key on
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
                CheckViolation::PhantomMergeDriver {
                    repo,
                    pattern,
                    driver,
                } => {
                    // No fix: `rwv-ours` is a guess at what the operator meant,
                    // and deleting the line is a guess that they meant nothing.
                    safe_to_fix = false;
                    (
                        crate::integration::Severity::Warning,
                        format!(
                            "{repo}: .gitattributes assigns merge driver `{driver}` to \
                             `{pattern}`, but rwv defines no driver by that name — the \
                             `rwv-` prefix is rwv's, so nothing else defines it either \
                             and the line silently does nothing (git falls back to a \
                             textual merge). Use `merge={defined}` for a derived path \
                             whose target-side copy should win, or drop the line",
                            defined = crate::git::RWV_MERGE_DRIVER_NAME,
                        ),
                    )
                }
            };
            Some(Issue {
                kind: IssueKind::CoreFinding,
                integration: "core".into(),
                severity,
                message,
                safe_to_fix,
            })
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
    // One dirty/clean read covers both classifiers, which is the whole point
    // of this helper: in a healthy workspace almost every repo is clean and
    // answers with a single subprocess. A read that fails is not evidence of
    // cleanliness, so it falls through to the per-kind classifiers, which are
    // already defensive about transient git failures.
    if crate::git::repo_is_dirty(repo) == Some(false) {
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
    match crate::git::index_tree_state(repo) {
        crate::git::IndexTreeState::MatchesHead => None,
        crate::git::IndexTreeState::RecentAncestorTree => Some(IndexDriftKind::SafeToFix),
        crate::git::IndexTreeState::Unrecognized => Some(IndexDriftKind::LiveStaged),
    }
}

/// Reset the index to match HEAD, leaving the working tree and HEAD untouched.
///
/// Only call after confirming `classify_index_drift` returns `SafeToFix`.
/// Uses bare `git reset` (equivalent to `git reset --mixed HEAD`).
pub fn reset_index_to_head(repo: &Path) -> anyhow::Result<()> {
    crate::git::reset_index(repo)
        .with_context(|| format!("failed to reset the index in {}", repo.display()))
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
/// - the `gitdir:` target's clone exists on disk (normal case)
/// - the `gitdir:` line cannot be parsed (defensive: caller should not skip
///   drift classification for unknowns)
///
/// The store is resolved from `commondir` when git's record of it survives,
/// and from the `<store>/worktrees/<name>` layout when it does not. That
/// second arm is not a shortcut: the `commondir` file lives inside the
/// canonical clone, so it is gone in precisely the situation this function
/// exists to detect, and a resolution that only reads the filesystem answers
/// `None` exactly when a finding is due.
pub fn worktree_canonical_clone_missing(repo: &Path) -> Option<PathBuf> {
    let git_dir = match crate::git::git_dir_link(repo)? {
        crate::git::GitDirLink::Owned(_) => return None,
        crate::git::GitDirLink::Linked(git_dir) => git_dir,
    };
    let store = crate::git::commondir_target(&git_dir)
        .or_else(|| crate::git::store_above_worktree_dir(&git_dir))?;
    // The reported path names the repo directory rather than its store, so it
    // matches DanglingReference's repair target.
    let canonical = store.parent()?;
    if canonical.exists() {
        None
    } else {
        Some(canonical.to_path_buf())
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
    // A probe that cannot answer classifies as `LiveEdits`, which is what
    // stops an auto-fix from overwriting content nothing inspected. The
    // canonical-clone-missing case is pre-classified upstream via
    // `worktree_canonical_clone_missing`, so that root cause does not reach
    // here in practice.
    match crate::git::working_tree_state(repo) {
        crate::git::WorkingTreeState::MatchesHead => None,
        crate::git::WorkingTreeState::RestorableFromHead(_) => {
            Some(WorkingTreeDriftKind::SafeToFix)
        }
        crate::git::WorkingTreeState::Unrecognized => Some(WorkingTreeDriftKind::LiveEdits),
    }
}

/// Restore working-tree files to match HEAD.
///
/// Only call after confirming `classify_working_tree_drift` returns `SafeToFix`.
/// Restores each tracked file that differs from HEAD using
/// `git checkout HEAD -- <files>`, leaving unstaged files and the index alone.
pub fn restore_working_tree_to_head(repo: &Path) -> anyhow::Result<()> {
    let files = crate::git::paths_differing_from_head(repo)
        .with_context(|| format!("failed to list drifted paths in {}", repo.display()))?;
    crate::git::restore_paths_from_head(repo, &files)
        .with_context(|| format!("failed to restore drifted paths in {}", repo.display()))
}

/// Execute `rwv doctor --locked` for the current workspace context.
///
/// Compares each repo's HEAD SHA against its `rwv.lock` entry. Outputs per-repo
/// status to stdout. Returns `Ok(true)` if any repo's tip differs from its lock
/// entry (exit 1), `Ok(false)` if all match (exit 0).
///
/// Total over the **raw** lock, which is what separates it from the pipeline's
/// [`CheckViolation::StaleLock`] rather than making it a second spelling of it:
/// an entry whose repo is absent from disk gets a verdict here and is
/// unreachable there, because [`crate::manifest::LockFile::resolve_versions`]
/// drops it and [`find_violations`] has no disk to re-read. Where both surfaces
/// do see an entry they must name the same two revisions in the same spelling —
/// pinned in `tests/lock_totality_agreement_test.rs`, which is also the only
/// coverage of the divergence.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_check_locked(ctx: &crate::workspace::WorkspaceContext) -> anyhow::Result<bool> {
    use crate::manifest::Project;
    use crate::vcs::{probe_vcs, vcs_for};
    use crate::workspace::Checkout;

    let workspace_dir = ctx.active_path().to_path_buf();

    let project_names: Vec<ProjectName> = match &ctx.checkout {
        Checkout::Primary { project: Some(p) } => vec![p.clone()],
        Checkout::Workweave { project, .. } => vec![project.clone()],
        Checkout::Primary { project: None } => crate::workspace::discover_projects(&workspace_dir),
    };

    let mut any_drift = false;

    for pname in &project_names {
        let project_dir = project_dir(&workspace_dir, pname.as_str());
        let project = match Project::from_dir(&project_dir, pname.clone()) {
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
            // The lock names paths; the manifest names backends. A lock entry
            // with no manifest entry has no declared backend to resolve from.
            let vcs = project
                .manifest
                .get_entry(repo_path)
                .map(|e| vcs_for(e.vcs_type))
                .unwrap_or_else(probe_vcs);
            let repo_abs = workspace_dir.join(repo_path.as_path());

            let actual = match vcs.head_revision(&repo_abs) {
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

/// Repairs that have to land before the workspace is read. A legacy marker
/// leaves a workweave unusable, so migrating it is what lets the rest of the
/// run resolve context at all.
///
/// The `[fixed]` announcement each arm prints is a checked surface, not
/// chatter: tests/doctor_fix_authority_test.rs binds every announcement in
/// this file to a finding [`CheckViolation::fix_disposition`] marks Auto or
/// Consented. An arm added here for a report-only finding fails there
/// instead of running unsanctioned.
fn apply_prelude_repairs(ctx: &crate::workspace::WorkspaceContext, fix_errors: &mut Vec<String>) {
    for finding in scan_for_legacy_workweave_markers(ctx.primary_path()) {
        match fix_legacy_workweave_marker(&finding) {
            Ok(true) => println!(
                "[fixed] core: migrated legacy workweave marker {} to JSON",
                finding.marker_path.display()
            ),
            Ok(false) => {}
            Err(e) => fix_errors.push(format!("legacy workweave marker fix failed: {e}")),
        }
    }
}

/// `--fix` arms that repair the workspace rather than a collected finding.
///
/// They run before [`collect_doctor_violations`], which is what lets a
/// repaired workspace report as healthy instead of surfacing both `[fixed]`
/// and the warning it just resolved. Within this function the order is load
/// bearing twice over: the pre-flat receipt arm must precede the branch-model
/// migration so it inspects only receipts that predate this run, and the
/// reattach must precede it too, since a store that has just been reattached
/// is no longer a detached finding.
///
/// Every arm through the migration reads state the workspace holds in exactly
/// one place — receipts live only in primary's `.rwv-workweave-index`, and the
/// refs they describe live in the one physical refdb every linked worktree
/// shares — so they take `primary_path()` and ignore which weave invoked
/// doctor. Arms repairing per-weave state must take `active_path()` instead.
///
/// This pass runs before collection and names no [`CheckViolation`], so the
/// runtime disposition gate in [`apply_finding_repairs`] never sees it. What
/// holds it to the register instead is the `[fixed]` announcement each arm
/// prints: tests/doctor_fix_authority_test.rs binds every announcement to a
/// finding the register marks Auto or Consented, so an arm added here for a
/// report-only finding is a red test rather than dead code.
fn apply_workspace_repairs(
    ctx: &crate::workspace::WorkspaceContext,
    world: &DoctorWorld,
    reattach: Option<crate::cli::consent::ReattachConsent>,
    adopt_detached: Option<crate::cli::consent::AdoptDetachedConsent>,
    fix_errors: &mut Vec<String>,
) {
    use crate::workspace::read_active_project;

    let vcs = world.vcs.as_ref();
    let project_scope = world.project_scope();

    if let Some(active_name) = read_active_project(ctx.primary_path()) {
        let project_dir = project_dir(ctx.primary_path(), active_name.as_str());
        if !project_dir.is_dir() {
            match crate::workspace::clear_active_project(ctx.primary_path()) {
                Ok(()) => println!(
                    "[fixed] core: cleared `.rwv-active` (was pointing at missing project `{}`)",
                    active_name
                ),
                Err(e) => fix_errors.push(format!(
                    "dangling-active-project fix failed for `{}`: {e}",
                    active_name
                )),
            }
        }
    }

    // Only the registered-workweave arm is repairable — see
    // `WeaveRootIdentityConflictKind` for why the split is not symmetric.
    for v in scan_weave_root_identity(ctx.primary_path(), ctx.active_path()) {
        if let CheckViolation::WeaveRootIdentityConflict {
            root,
            sub_kind:
                WeaveRootIdentityConflictKind::RegisteredWorkweave {
                    project,
                    workweave_name,
                },
            ..
        } = &v
        {
            match crate::workspace::clear_active_project(root) {
                Ok(()) => println!(
                    "[fixed] core: deleted `.rwv-active` at {} \
                     (redundant with the `.rwv-workweave` marker of registered workweave \
                     `{workweave_name}` in project `{project}`; the marker is unchanged)",
                    root.display()
                ),
                Err(e) => fix_errors.push(format!(
                    "weave-root-identity-conflict fix failed for {}: {e}",
                    root.display()
                )),
            }
        }
    }

    let (retracted, retract_errs) = fix_dangling_receipts(ctx.primary_path(), vcs, project_scope);
    for (store_path, ref_name) in &retracted {
        println!(
            "[fixed] core: retracted dangling ownership receipt for `{}` in {}",
            ref_name,
            store_path.display()
        );
    }
    fix_errors.extend(retract_errs);

    let (retracted, retract_errs) = fix_pre_flat_receipts(ctx.primary_path(), project_scope);
    for (store_path, ref_name) in &retracted {
        println!(
            "[fixed] core: retracted the ownership receipt for `{}` in {} — the name \
             carries a `/` segment, which no workweave on disk mints; the branch \
             itself was left untouched and is now unowned",
            ref_name,
            store_path.display()
        );
    }
    fix_errors.extend(retract_errs);

    // Adding the `receipts` field is the migration's precondition, not one of
    // its arms: `RefRegistry::record_created` refuses against an index without
    // it. The registry it produces is empty, so every pre-existing ref stays
    // unowned until an arm records it.
    for project in crate::workspace::discover_projects(ctx.primary_path()) {
        if let Some(active) = project_scope {
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
            Err(e) => fix_errors.push(format!(
                "failed to migrate {}'s workweave index to the ref-ownership registry: {e}",
                project
            )),
        }
    }

    if let Some(consent) = reattach {
        let (reattached, reattach_errs) = fix_detached_canonicals(
            ctx.primary_path(),
            vcs,
            &world.input.projects,
            project_scope,
            &world.input.known_repos,
            consent,
        );
        for (store_path, branch) in &reattached {
            println!(
                "[fixed] core: reattached detached canonical {} to `{}`",
                store_path.display(),
                branch
            );
        }
        fix_errors.extend(reattach_errs);
    }

    let (applied, migration_errs) =
        fix_branch_model_migration(ctx.primary_path(), vcs, project_scope, adopt_detached);
    for msg in &applied {
        println!("[fixed] core: {msg}");
    }
    fix_errors.extend(migration_errs);

    for project in &world.input.projects {
        let project_repo = project_dir(&world.workspace_dir, project.name.as_str());
        if !project_repo.is_dir() {
            continue;
        }
        let Ok(state) =
            vcs.replay_exclusion_state(&project_repo, std::path::Path::new(LockFile::FILE_NAME))
        else {
            continue;
        };
        let Some(sub_kind) = replay_exclusion_finding(state) else {
            continue;
        };
        let migration = match sub_kind {
            ReplayExclusionKind::Absent => None,
            ReplayExclusionKind::LegacySpelling => {
                Some("migrated `rwv.lock merge=ours` → `rwv.lock merge=rwv-ours`")
            }
            ReplayExclusionKind::LegacyAlongsideCurrent => Some(
                "dropped the legacy `rwv.lock merge=ours` line, leaving `rwv.lock merge=rwv-ours`",
            ),
        };
        // Rewrites a legacy line to the new name in place rather than
        // appending alongside it, and is a no-op once the new line is the
        // only one.
        match vcs.set_replay_exclusion(&project_repo, std::path::Path::new(LockFile::FILE_NAME)) {
            Ok(()) => {
                if let Some(migration) = migration {
                    // The invariant reads the *committed* form, so a migration
                    // that stops at the working tree is not yet in effect. A
                    // repo with unrelated staged work is left uncommitted —
                    // user work is never bundled with an rwv-authored fix.
                    match commit_replay_exclusion_migration(vcs, &project_repo) {
                        Ok(CommitOutcome::Committed) => println!(
                            "[fixed] core: {migration} in {}/.gitattributes (committed)",
                            project.name
                        ),
                        Ok(CommitOutcome::SkippedUnrelatedStaged) => println!(
                            "[fixed] core: {migration} in {}/.gitattributes (NOT committed: \
                             project repo has unrelated staged changes; commit them, then \
                             re-run `rwv doctor --fix` to complete the migration)",
                            project.name
                        ),
                        Ok(CommitOutcome::NothingToCommit) => println!(
                            "[fixed] core: {migration} in {}/.gitattributes",
                            project.name
                        ),
                        Err(e) => fix_errors.push(format!(
                            "{}: migrated .gitattributes but commit failed: {e}",
                            project.name
                        )),
                    }
                } else {
                    println!(
                        "[fixed] core: wrote `rwv.lock merge=rwv-ours` to {}/.gitattributes",
                        project.name
                    );
                }
            }
            Err(e) => fix_errors.push(format!(
                "{}: failed to write replay-exclusion: {e}",
                project.name
            )),
        }
    }

    for project in &world.input.projects {
        let project_repo = project_dir(&world.workspace_dir, project.name.as_str());
        if !project_repo.is_dir() {
            continue;
        }
        if let Ok(false) = crate::git::has_rwv_merge_driver_config(&project_repo) {
            match crate::git::plant_rwv_merge_driver_config(&project_repo) {
                Ok(()) => println!(
                    "[fixed] core: planted `{}` config in {}",
                    crate::git::RWV_MERGE_DRIVER_CONFIG_KEY,
                    project.name
                ),
                Err(e) => fix_errors.push(format!(
                    "{}: failed to plant `{}`: {e}",
                    project.name,
                    crate::git::RWV_MERGE_DRIVER_CONFIG_KEY
                )),
            }
        }
    }
}

/// `--fix` arms that act on a collected finding rather than on the workspace.
///
/// A repaired finding is dropped from the returned vector so the operator is
/// never shown both `[fixed]` and the warning it resolved. A repair that
/// errored is dropped too — the error itself is the report.
///
/// Nothing here can act on a finding [`CheckViolation::fix_disposition`] calls
/// report-only, so the register can narrow what this repairs but never widen
/// it.
fn apply_finding_repairs(
    ctx: &crate::workspace::WorkspaceContext,
    world: &DoctorWorld,
    violations: Vec<CheckViolation>,
    repo_locations: &std::collections::HashMap<
        (Option<WorkweaveName>, RepoPath),
        std::path::PathBuf,
    >,
    fix_errors: &mut Vec<String>,
) -> Vec<CheckViolation> {
    let vcs = world.vcs.as_ref();

    let location_of = |workweave: &Option<WorkweaveName>, repo: &RepoPath| match workweave {
        Some(ww) => format!("{ww}/{repo}"),
        None => format!("{repo}"),
    };

    // A stale worktree registration is what makes git refuse the branch
    // delete below: pruned here, ahead of fix_stale_ephemeral_branches,
    // rather than in the loop past it, so the delete never meets that
    // guard within the same --fix pass.
    let (registrations, violations): (Vec<_>, Vec<_>) = violations
        .into_iter()
        .partition(|v| matches!(v, CheckViolation::StaleWorktreeRegistration { .. }));

    let mut kept = Vec::with_capacity(violations.len() + registrations.len());
    for v in registrations {
        let CheckViolation::StaleWorktreeRegistration {
            workweave, repo, ..
        } = &v
        else {
            unreachable!()
        };
        let repaired = match repo_locations.get(&(workweave.clone(), repo.clone())) {
            Some(repo_abs) => match fix_state_hygiene(vcs, &v, repo_abs) {
                Ok(true) => {
                    println!(
                        "[fixed] core: stale-worktree-registration for {}: pruned",
                        location_of(workweave, repo)
                    );
                    true
                }
                Ok(false) => false,
                Err(e) => {
                    fix_errors.push(format!("state-hygiene --fix failed: {e}"));
                    true
                }
            },
            None => false,
        };
        if !repaired {
            kept.push(v);
        }
    }

    let (deleted, delete_errs) = fix_stale_ephemeral_branches(
        ctx.primary_path(),
        vcs,
        &world.input.projects,
        world.project_scope(),
        &world.input.known_repos,
    );
    for (repo_path, branch) in &deleted {
        println!(
            "[fixed] core: deleted safe-class stale ephemeral branch `{}` in {}",
            branch,
            repo_path.display()
        );
    }
    let deleted_keys: std::collections::HashSet<(PathBuf, String)> = deleted.into_iter().collect();
    fix_errors.extend(delete_errs);

    for v in violations {
        if matches!(v.fix_disposition(), FixDisposition::ReportOnly) {
            kept.push(v);
            continue;
        }
        let repaired = match &v {
            CheckViolation::BranchDiscipline {
                repo_path,
                sub_kind: BranchDisciplineKind::StaleEphemeralBranchSafe { branch, .. },
            } => deleted_keys.contains(&(repo_path.clone(), branch.clone())),

            // Repaired from the vector rather than before it, because the
            // weave-root-identity classification asks which workweaves the
            // registry vouches for. Adopting an unregistered tree first would
            // re-answer that question mid-run, and a tree the operator copied
            // out-of-band would be reported as one rwv had always known.
            CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir,
                sub_kind: WorkweaveTreeIntegrityKind::DanglingParent { .. },
            } => match fix_dangling_parent(workweave_dir, ctx.primary_path()) {
                Ok(true) => {
                    println!(
                        "[fixed] core: re-pointed dangling parent of {} to primary",
                        workweave_dir.display()
                    );
                    true
                }
                Ok(false) => true,
                Err(e) => {
                    fix_errors.push(format!("dangling-parent --fix failed: {e}"));
                    false
                }
            },

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
                let outcome = match crate::manifest::ProjectName::new(project.clone()) {
                    Ok(project_name) => {
                        fix_stale_registry_entry(ctx.primary_path(), &project_name, workweave_name)
                    }
                    Err(e) => Err(e.into()),
                };
                match outcome {
                    Ok(()) => {
                        println!(
                            "[fixed] core: pruned stale registry entry `{}` \
                             → {} in project `{}`",
                            workweave_name,
                            recorded_path.display(),
                            project
                        );
                        true
                    }
                    Err(e) => {
                        fix_errors.push(format!(
                            "workweave-index --fix failed: prune of stale entry `{}` in `{}` \
                             failed: {e}",
                            workweave_name, project
                        ));
                        false
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
                let outcome = match crate::manifest::ProjectName::new(project.clone()) {
                    Ok(project_name) => fix_unregistered_workweave(
                        ctx.primary_path(),
                        &project_name,
                        workweave_name,
                        workweave_dir,
                    ),
                    Err(e) => Err(e.into()),
                };
                match outcome {
                    Ok(()) => {
                        println!(
                            "[fixed] core: adopted workweave `{}` at {} into \
                             project `{}`'s registry",
                            workweave_name,
                            workweave_dir.display(),
                            project
                        );
                        true
                    }
                    Err(e) => {
                        fix_errors.push(format!(
                            "workweave-index --fix failed: adopt of workweave `{}` at {} \
                             failed: {e}",
                            workweave_name,
                            workweave_dir.display()
                        ));
                        false
                    }
                }
            }

            CheckViolation::IndexDrift {
                workweave,
                repo,
                kind: IndexDriftKind::SafeToFix,
            } => match repo_locations.get(&(workweave.clone(), repo.clone())) {
                Some(repo_abs) => {
                    let location = location_of(workweave, repo);
                    match reset_index_to_head(repo_abs) {
                        Ok(()) => println!("[fixed] core: index refreshed for {location}"),
                        Err(e) => fix_errors.push(format!("{location}: index fix failed: {e}")),
                    }
                    true
                }
                None => false,
            },

            CheckViolation::WorkingTreeDrift {
                workweave,
                repo,
                kind: WorkingTreeDriftKind::SafeToFix,
            } => match repo_locations.get(&(workweave.clone(), repo.clone())) {
                Some(repo_abs) => {
                    let location = location_of(workweave, repo);
                    match restore_working_tree_to_head(repo_abs) {
                        Ok(()) => println!("[fixed] core: working tree refreshed for {location}"),
                        Err(e) => {
                            fix_errors.push(format!("{location}: working-tree fix failed: {e}"))
                        }
                    }
                    true
                }
                None => false,
            },

            // The auto-fixable state-hygiene set; `fix_state_hygiene` carries
            // the policy for why the rest are left alone. Stale worktree
            // registrations are pruned in the pre-pass above, not here.
            CheckViolation::OrphanedSavepoint {
                workweave,
                repo,
                op_id,
                sub_kind: OrphanedSavepointKind::Redundant,
            } => match repo_locations.get(&(workweave.clone(), repo.clone())) {
                Some(repo_abs) => match fix_state_hygiene(vcs, &v, repo_abs) {
                    Ok(true) => {
                        println!(
                            "[fixed] core: orphaned-savepoint for {}: dropped op_id={op_id}",
                            location_of(workweave, repo)
                        );
                        true
                    }
                    Ok(false) => false,
                    Err(e) => {
                        fix_errors.push(format!("state-hygiene --fix failed: {e}"));
                        true
                    }
                },
                None => false,
            },

            // Operates on the lease's own workspace dir, so no repo lookup.
            CheckViolation::DeadOpLease {
                workspace_dir,
                op_id,
                ..
            } => match fix_state_hygiene(vcs, &v, workspace_dir) {
                Ok(true) => {
                    println!(
                        "[fixed] core: dead-op-lease for {}: removed lease (op_id={op_id})",
                        workspace_dir.display()
                    );
                    true
                }
                Ok(false) => false,
                Err(e) => {
                    fix_errors.push(format!("state-hygiene --fix failed: {e}"));
                    true
                }
            },

            _ => false,
        };
        if !repaired {
            kept.push(v);
        }
    }
    kept
}

/// Whether the integration pass repairs what it finds or only reports it.
///
/// `--fix` interleaves with this collection rather than following it: a
/// fixable `verify()` or surfacing finding is repaired in place and dropped
/// from the report, so the operator is never shown both `[fixed]` and the
/// warning it resolved. Under [`Repair::Report`] every finding is returned.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Repair {
    Report,
    Apply,
}

/// Every generated file in the loaded projects whose attested inputs no longer
/// describe the checkout, as the one read both doctor surfaces render.
///
/// The condition `rwv sync` announces once, standing. A note prints at a moment
/// the operator may not be reading and is then gone; this is derivable from
/// present state whenever they ask, and from this checkout alone — no source
/// workspace, no history.
fn stale_generation_findings(world: &DoctorWorld) -> Vec<crate::owned_state::StaleGeneration> {
    let mut findings = Vec::new();
    for project in &world.input.projects {
        let project_dir =
            crate::workspace::project_dir(&world.workspace_dir, project.name.as_str());
        findings.extend(crate::owned_state::stale_generations(
            &project_dir,
            &project.name,
            &world.workspace_dir,
        ));
    }
    findings
}

/// The operator's rendering of a stale generation.
fn stale_generation_issue(finding: &crate::owned_state::StaleGeneration) -> Issue {
    use crate::integration::Severity;

    let cause = if finding.provenance_unknown() {
        "it was accepted without a record of what produced it, so nothing here can \
         say it still follows from the current inputs"
            .to_string()
    } else {
        format!(
            "the inputs it was generated from have moved since: {}",
            finding.moved_inputs.join(", ")
        )
    };
    Issue {
        integration: "core".into(),
        severity: Severity::Warning,
        message: format!(
            "{} may no longer match this checkout — {cause}. Run `rwv materialize` \
             to re-derive it",
            finding.generated
        ),
        kind: IssueKind::DerivedStateStale,
        safe_to_fix: false,
    }
}

/// The same finding as the typed advisory `rwv sync --json` already emits, so
/// an agent branches on one vocabulary rather than two.
fn stale_generation_advisory(
    finding: &crate::owned_state::StaleGeneration,
) -> crate::workspace::AdvisoryOutput {
    crate::workspace::AdvisoryOutput {
        kind: crate::workspace::AdvisoryKindOutput::DerivedStateStale,
        remedy: "rwv materialize".to_owned(),
        inputs: finding.moved_inputs.clone(),
    }
}

/// One finding per disabled integration whose content is still on disk.
///
/// **Report only, and there is deliberately no `--fix` arm.** Reaching the state
/// a disabled integration implies means deleting what it authored, and the edit
/// that disables an integration is a one-character change in `rwv.toml` — a
/// typo that put artifact deletion one `--fix` away would be a repair verb with
/// a blast radius nobody asked for. The named remedy is `rwv materialize`,
/// which is the operator saying it in as many words.
fn disabled_integration_issues(
    workspace_dir: &Path,
    project: &crate::manifest::ProjectName,
    integrations: &[&dyn crate::integration::Integration],
    manifest: &crate::manifest::Manifest,
    ctx_base: &crate::integration_runner::IntegrationContextBase,
) -> Vec<Issue> {
    use crate::integration::{OwnedPath, Severity};

    crate::integration_runner::disabled_integration_artifacts(integrations, manifest, ctx_base)
        .into_iter()
        .map(|found| {
            let described = found
                .paths
                .iter()
                .map(|path| {
                    let shape = match path {
                        OwnedPath::WholeFile(_) => "generated file",
                        OwnedPath::MarkedRegion(_) => "managed region",
                    };
                    format!(
                        "{} ({shape})",
                        ctx_base.output_dir.join(path.name()).display()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let names: Vec<String> = found.paths.iter().map(|p| p.name().to_string()).collect();
            let surfaced = crate::activate::surfaced_names(workspace_dir, &names);
            let also = if surfaced.is_empty() {
                String::new()
            } else {
                format!(", surfaced at the weave root as {}", surfaced.join(", "))
            };
            Issue {
                integration: found.integration.clone(),
                severity: Severity::Warning,
                message: format!(
                    "{} is disabled for project `{project}`, so nothing here is its \
                     to keep, but content it authored is still on disk: {described}{also}. \
                     Run `rwv materialize` to remove it. `rwv doctor --fix` will not — \
                     removal is not a repair, and disabling an integration is one \
                     character in rwv.toml",
                    found.integration
                ),
                kind: IssueKind::DisabledIntegrationArtifact,
                safe_to_fix: false,
            }
        })
        .collect()
}

/// Every finding an integration raised, for one pass over the loaded projects.
///
/// The counterpart of [`collect_doctor_violations`] on the other channel, and
/// for the same reason: both renderers take their `Issue`s from here, so a
/// hook reachable from one is reachable from the other by construction.
///
/// Under [`Repair::Report`] nothing here mints an [`IssueKind::CoreFinding`] —
/// every core finding is a `CheckViolation` — which is what lets `--json`
/// carry the two channels as disjoint arrays.
fn collect_doctor_issues(world: &DoctorWorld, repair: Repair) -> Vec<Issue> {
    use crate::integration::Severity;
    use crate::integration_runner::run_checks;

    let workspace_dir = &world.workspace_dir;
    let builtin = crate::integrations::builtin_integrations();
    let integrations: Vec<&dyn crate::integration::Integration> =
        builtin.iter().map(|b| b.as_ref()).collect();
    let fix = repair == Repair::Apply;

    let mut issues: Vec<Issue> = Vec::new();
    for project in &world.input.projects {
        let detection_cache = crate::integration_runner::build_detection_cache(
            &integrations,
            workspace_dir,
            project.manifest.iter_entries(),
        );
        let ctx_base = world.session.context_base(
            &project.name,
            &detection_cache,
            project.manifest.workweave.as_ref(),
        );
        issues.extend(run_checks(&integrations, &project.manifest, &ctx_base));
        issues.extend(disabled_integration_issues(
            workspace_dir,
            &project.name,
            &integrations,
            &project.manifest,
            &ctx_base,
        ));

        // The integrations' `verify()` pass reports drift between on-disk
        // managed content and what `activate()` would produce. USER-HELD
        // findings (`safe_to_fix = false`) surface even under `--fix`: the
        // user holds the pen on that file region and auto-repair would
        // silently destroy their content.
        let verify_issues = crate::integration_runner::run_verifications(
            &integrations,
            &project.manifest,
            &ctx_base,
        );
        let (fixable_issues, user_held_issues): (Vec<_>, Vec<_>) =
            verify_issues.into_iter().partition(|i| i.safe_to_fix);
        // Withholding is per project, because the repair is: activation runs
        // every managed file's hooks, and a hook re-attests what it produces —
        // so a user-held finding anywhere in the project is content this
        // repair settles without being told which way to settle it. The
        // per-finding `safe_to_fix` keeps `--fix` off the finding itself and
        // says nothing about a repair entered for a different one; the drift
        // refusal that would catch it lives in activation's materialize mode,
        // and this is its intent mode.
        let user_held_content = !user_held_issues.is_empty();
        issues.extend(user_held_issues);
        if fix && !fixable_issues.is_empty() && !user_held_content {
            // The repair primitive must be pointed at the same weave the
            // detector scanned. `activate_intent` targets primary
            // unconditionally, so from a workweave it would rewrite the
            // PRIMARY project's managed files — `activate_intent_at` takes the
            // weave dir, and `workspace_dir` is `ctx.active_path()`.
            match crate::activate::activate_intent_at(project.name.as_str(), workspace_dir) {
                Ok(()) => println!(
                    "[fixed] core: regenerated integration content for project `{}` (drift detected)",
                    project.name
                ),
                Err(e) => issues.push(Issue {
                    kind: IssueKind::CoreFinding,
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
            if fix && !fixable_issues.is_empty() {
                issues.push(Issue {
                    kind: IssueKind::CoreFinding,
                    integration: "core".into(),
                    severity: Severity::Warning,
                    message: format!(
                        "doctor --fix: withheld the regeneration of `{}`'s integration \
                         content. Regenerating re-runs the hooks for every managed file \
                         the project has, and they re-attest what they produce — which \
                         settles the drift reported above without the consent that says \
                         which way. Choose it: `rwv materialize --adopt-drifted` records \
                         the current content as accepted, `rwv materialize \
                         --regenerate-drifted` discards it and regenerates from the \
                         current inputs. Then re-run `rwv doctor --fix`",
                        project.name
                    ),
                    safe_to_fix: false,
                });
            }
            issues.extend(fixable_issues);
        }

        // An `Ownership::DefaultOnly` value the operator holds may be
        // incompatible with what the members require, which `verify()` does
        // not see. No automated repair exists, so these bypass the `--fix`
        // partition above.
        issues.extend(crate::integration_runner::run_member_incompatibilities(
            &integrations,
            &project.manifest,
            &ctx_base,
        ));

        // Axis-1: nothing in `verify()` asserts that the surfacing *symlinks*
        // exist and resolve. Scoped to `workspace_dir` (= `ctx.active_path()`),
        // so it checks primary's surfacing at primary and the workweave's
        // inside a workweave.
        let surfacing_issues =
            crate::activate::verify_surfacing(workspace_dir, &project.name, &project.manifest);
        let (surf_fixable, surf_user_held): (Vec<_>, Vec<_>) =
            surfacing_issues.into_iter().partition(|i| i.safe_to_fix);
        // A real file or dir occupying a surfacing path is user-held and never
        // auto-clobbered.
        issues.extend(surf_user_held);
        if fix && !surf_fixable.is_empty() {
            // Authors no content — it only (re)creates the owner-scoped
            // symlinks, which is what workweave-create runs at creation.
            // `--project` scopes doctor to a project without switching to it,
            // so the weave root's shared names stay with the project it
            // presents.
            match crate::activate::surface_symlinks(
                workspace_dir,
                &project.name,
                &project.manifest,
                crate::activate::SurfacingMode::Repair,
            ) {
                Ok(()) => println!(
                    "[fixed] core: re-surfaced symlinks for project `{}` (missing/mis-resolved surfacing)",
                    project.name
                ),
                Err(e) => issues.push(Issue {
                    kind: IssueKind::CoreFinding,
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
            issues.extend(surf_fixable);
        }
    }
    issues.extend(
        stale_generation_findings(world)
            .iter()
            .map(stale_generation_issue),
    );
    issues
}

/// Execute `rwv doctor` for the current workspace context.
///
/// Scans registry directories for repos on disk, loads all project manifests,
/// runs convention checks and integration check hooks, then displays issues.
/// The `--kind` report filter: a validated set of finding kinds the doctor
/// report is narrowed to, text and `--json` both.
///
/// Kind names are the wire spellings — the `kind` tag each violation
/// serializes under in `rwv doctor --json`, the same names the per-class
/// count lines and the published schema carry.
///
/// **An unknown name refuses, naming the valid set.** A filter that
/// silently matched nothing would render an empty report, and an empty
/// doctor report reads as "clean" — the one thing a typo must never
/// produce. The valid set is derived from the wire type's own JSON schema
/// ([`ViolationOutput`] via schemars) rather than maintained beside it, so
/// the refusal message cannot drift from what `--json` actually emits.
///
/// **The filtered view is the drill-down.** Kinds whose classes normally
/// collapse to a per-class count line render itemized under the filter —
/// `--kind` exists so triage does not require `--json | jq`, so it shows
/// the records the count line summarized. Integration issues are not
/// violations of any kind and are absent from a kind-filtered view, and
/// the exit code reflects the filtered view: an error outside the named
/// kinds does not fail a `--kind` run.
pub struct KindFilter {
    kinds: std::collections::BTreeSet<String>,
}

impl KindFilter {
    /// Every `kind` name `rwv doctor --json` can emit, sorted — read off
    /// the wire type's schema, which is generated from the serialized type
    /// and therefore is the register.
    pub fn valid_kinds() -> Vec<String> {
        let schema = schemars::schema_for!(ViolationOutput);
        let mut kinds: Vec<String> = schema
            .schema
            .subschemas
            .as_ref()
            .and_then(|s| s.one_of.as_ref())
            .map(|variants| {
                variants
                    .iter()
                    .filter_map(|v| {
                        let schemars::schema::Schema::Object(obj) = v else {
                            return None;
                        };
                        obj.object.as_ref()?.properties.get("kind").and_then(|k| {
                            let schemars::schema::Schema::Object(k) = k else {
                                return None;
                            };
                            k.enum_values.as_ref()?.first()?.as_str().map(String::from)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        kinds.sort();
        kinds
    }

    /// Build a filter from `--kind` values, refusing any name the wire
    /// vocabulary does not contain.
    pub fn new(names: &[String]) -> anyhow::Result<Self> {
        let valid: std::collections::BTreeSet<String> = Self::valid_kinds().into_iter().collect();
        let mut kinds = std::collections::BTreeSet::new();
        for name in names {
            if !valid.contains(name) {
                anyhow::bail!(
                    "`--kind={name}` names no doctor finding kind. Valid kinds:\n  {}",
                    valid.iter().cloned().collect::<Vec<_>>().join("\n  ")
                );
            }
            kinds.insert(name.clone());
        }
        Ok(Self { kinds })
    }

    /// Whether `v` belongs to one of the named kinds.
    fn admits(&self, v: &CheckViolation) -> bool {
        self.kinds.contains(v.wire_kind())
    }
}

/// When `fix` is `true`, the repairable subset is remediated in place before
/// the report is rendered, so a repaired workspace reports healthy.
///
/// Both of its inputs are collected elsewhere and shared with
/// [`run_check_json`]: `collect_doctor_violations` for the core findings and
/// `collect_doctor_issues` for the integrations'. The only thing this
/// function raises itself is a `--fix` arm's own failure, which `--json` has no
/// `--fix` to produce.
///
/// When `scope_all` is `false` (the default), only the active project is
/// checked: stale locks, dangling references, and integration hooks are
/// scoped to that project, and orphan detection is skipped (a repo absent
/// from the active project may belong to another project). Pass `scope_all =
/// true` (via `--all`) to reproduce the weave-wide behaviour, including orphan
/// detection across every project.
///
/// Returns `Ok(true)` if there are errors (exit 1), `Ok(false)` if clean.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
///
/// `reattach` is the [`ReattachConsent`] the CLI minted from
/// `--reattach-checkouts`, or `None` when the operator did not pass it.
/// It gates exactly one thing: whether `--fix` *reattaches* a detached
/// canonical store or only reports
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
    kind_filter: Option<&KindFilter>,
) -> anyhow::Result<bool> {
    use crate::integration::Severity;

    // The P2 gate: a binary whose release removed migratory arms refuses a
    // weave whose recorded floor predates them, naming the bridge version.
    // A no-op while no removal has named a requirement.
    crate::health_floor::enforce(ctx.primary_path())?;

    let mut fix_errors: Vec<String> = Vec::new();

    if fix {
        apply_prelude_repairs(ctx, &mut fix_errors);
    }

    let world = load_doctor_world(ctx, scope_all)?;

    if fix {
        apply_workspace_repairs(ctx, &world, reattach, adopt_detached, &mut fix_errors);
    }

    let DoctorFindings {
        violations,
        repo_locations,
        ..
    } = collect_doctor_violations(ctx, &world, ScanProgress::Heartbeat);

    let violations = if fix {
        apply_finding_repairs(ctx, &world, violations, &repo_locations, &mut fix_errors)
    } else {
        violations
    };

    // Read before `violations` moves into rendering. A violation holds
    // the health floor back only while rwv itself could repair it: every
    // migratory arm the floor licenses removing is an Auto or Consented
    // repair, so requiring that set discharged is what keeps the
    // attestation honest. A report-only warning is an observatory over
    // state that is not always this weave's to clear — version skew
    // across sovereign repos, submodules in a reference checkout, a
    // sibling weave's workweaves in the shared container — and blocking
    // on those makes such a weave permanently floorless. Error-severity
    // findings, converted violations included, block through
    // `has_errors` below.
    let violations_all_report_only = violations
        .iter()
        .all(|v| matches!(v.fix_disposition(), FixDisposition::ReportOnly));

    let mut all_issues = match kind_filter {
        // The filtered view is the drill-down: the named kinds render
        // ITEMIZED, bypassing the per-class count collapse — `--kind` is
        // what replaces `--json | jq` for triage, so it must show the
        // records, not the count line that pointed here. Integration
        // issues are not violations of any kind and are out of a
        // kind-filtered view entirely.
        Some(filter) => itemized_violations_to_issues(
            violations
                .into_iter()
                .filter(|v| filter.admits(v))
                .collect(),
        ),
        None => violations_to_issues(violations),
    };

    for msg in fix_errors {
        all_issues.push(Issue {
            kind: IssueKind::CoreFinding,
            integration: "core".into(),
            severity: Severity::Error,
            message: msg,
            safe_to_fix: true,
        });
    }

    if kind_filter.is_none() {
        all_issues.extend(collect_doctor_issues(
            &world,
            if fix { Repair::Apply } else { Repair::Report },
        ));
    }

    let mut has_errors = false;
    for issue in &all_issues {
        let prefix = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => {
                has_errors = true;
                "error"
            }
        };
        println!("[{prefix}] {}: {}", issue.integration, issue.message);
    }

    // The P1 record: a clean weave-wide unfiltered run advances the health
    // floor. Weave-wide because the floor licenses arm removal for the
    // whole weave and a scoped run proves nothing beyond its project;
    // unfiltered because `--kind` narrows what was even looked at. Clean
    // means no error-severity finding and no violation rwv can still
    // repair; report-only warnings do not block, per the classification
    // where `violations_all_report_only` is computed. Best-effort: a
    // floor is a record, not a precondition of the run that earned it.
    if scope_all && kind_filter.is_none() && violations_all_report_only && !has_errors {
        if let Err(e) =
            crate::health_floor::record_clean_run(ctx.primary_path(), world.vcs.as_ref())
        {
            eprintln!("warning: could not record the health floor: {e:#}");
        }
    }

    Ok(has_errors)
}

/// Build the payload for `rwv doctor --json` from a vector of violations and
/// the resolved workspace context. Extracted from [`run_check_json`] so tests
/// can drive the serialization shape without reaching for a real workspace on
/// disk.
pub fn build_doctor_json(
    violations: Vec<CheckViolation>,
    issues: Vec<Issue>,
    workspace_dir: &Path,
    workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
    resolution: Option<Resolution>,
    plugins: Vec<crate::plugins::PluginRecord>,
    advisories: Vec<crate::workspace::AdvisoryOutput>,
) -> DoctorJsonOutput {
    DoctorJsonOutput {
        schema_url: DOCTOR_SCHEMA_URL.to_owned(),
        violations: violations
            .into_iter()
            .map(|v| ViolationOutput::from_violation(v, workspace_dir, workweave_dirs))
            .collect(),
        issues: issues.into_iter().map(IssueOutput::from_issue).collect(),
        advisories,
        plugins,
        resolution,
    }
}

/// Everything `rwv doctor` reads off disk before any scan runs: the workspace
/// session, the loaded manifests, and each on-disk repo's HEAD.
///
/// `head_read_failures`, `lock_resolve_failures` and `projects_dir_read_error`
/// are raised as findings by [`collect_doctor_violations`] rather than here,
/// so both renderers see them on the same terms as every other scan.
struct DoctorWorld {
    workspace_dir: PathBuf,
    session: crate::workspace::WorkspaceSession,
    vcs: Box<dyn crate::vcs::Vcs>,
    active_project: Option<crate::manifest::ProjectName>,
    scope_all: bool,
    input: CheckInput,
    unparseable_projects: Vec<(crate::manifest::ProjectName, PathBuf, String)>,
    head_read_failures: Vec<(RepoPath, String)>,
    lock_resolve_failures: Vec<(crate::manifest::ProjectName, RepoPath)>,
    /// `Some((path, error))` when `projects/` exists but could not be listed.
    /// `scan_projects` swallows exactly this error and returns an empty
    /// list, which is indistinguishable downstream from a workspace that
    /// genuinely has no projects — this is the probe that tells the two apart
    /// before that swallowing happens.
    projects_dir_read_error: Option<(PathBuf, String)>,
    /// The walk of `projects/` the loaded projects came from. Its other two
    /// lists are the directories sitting there that rwv does not read as a
    /// project, and each is raised as a finding rather than passed over.
    project_scan: crate::workspace::ProjectScan,
}

impl DoctorWorld {
    /// `Some(name)` restricts a scan to one project's registry or findings;
    /// `None` visits every project. A workspace with no active project takes
    /// the weave-wide path even without `--all`, because there is nothing to
    /// narrow to.
    fn project_scope(&self) -> Option<&str> {
        if self.scope_all {
            None
        } else {
            self.active_project.as_ref().map(|n| n.as_str())
        }
    }
}

fn load_doctor_world(
    ctx: &crate::workspace::WorkspaceContext,
    scope_all: bool,
) -> anyhow::Result<DoctorWorld> {
    use crate::manifest::Project;
    use crate::workspace::WorkspaceSession;

    let workspace_dir = ctx.active_path().to_path_buf();
    let session = WorkspaceSession::new(&workspace_dir);
    let vcs = crate::vcs::probe_vcs();
    let active_project: Option<crate::manifest::ProjectName> = ctx.active_project().cloned();

    let mut head_revisions = BTreeMap::new();
    let mut head_read_failures: Vec<(RepoPath, String)> = Vec::new();
    for repo_path in session.repos_on_disk() {
        let abs = workspace_dir.join(repo_path.as_path());
        match vcs.head_revision(&abs) {
            Ok(rev) => {
                head_revisions.insert(repo_path.clone(), rev);
            }
            Err(e) => head_read_failures.push((repo_path.clone(), e.to_string())),
        }
    }

    let projects_dir_path = projects_dir(&workspace_dir);
    let projects_dir_read_error = match std::fs::read_dir(&projects_dir_path) {
        Ok(_) => None,
        // A workspace with no `projects/` at all yet (before the first `rwv
        // add`) is a different, unremarkable state — only a directory that
        // exists and refuses to be listed is the finding.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some((projects_dir_path, e.to_string())),
    };

    let mut projects = Vec::new();
    let mut known_repos = BTreeSet::new();
    let mut lock_resolve_failures: Vec<(crate::manifest::ProjectName, RepoPath)> = Vec::new();
    let mut unparseable_projects: Vec<(crate::manifest::ProjectName, PathBuf, String)> = Vec::new();
    let mut resolved_locks: std::collections::HashMap<
        crate::manifest::ProjectName,
        crate::manifest::ResolvedLockFile,
    > = std::collections::HashMap::new();

    let project_scan = crate::workspace::scan_projects(&workspace_dir);

    for name in &project_scan.projects {
        let dir = project_dir(&workspace_dir, name.as_str());
        let manifest_path = dir.join(Manifest::FILE_NAME);

        if !scope_all {
            if let Some(ref active) = active_project {
                if name != active {
                    continue;
                }
            }
        }

        match Project::from_dir(&dir, name.clone()) {
            Ok(project) => {
                // Resolving against the on-disk repos is what makes the
                // canonical-SHA equality in `find_violations` work
                // uniformly for tag-form, branch-form and SHA-form locks.
                // An entry that will not resolve is kept as a failure
                // rather than dropped: dropping it leaves the repo with no
                // `head_revisions` entry, which reads as healthy.
                if let Some(raw_lock) = project.lock.clone() {
                    let (resolved, failures) = raw_lock.resolve_versions(&workspace_dir);
                    for (unresolved, _raw_rev) in failures {
                        lock_resolve_failures.push((project.name.clone(), unresolved));
                    }
                    resolved_locks.insert(project.name.clone(), resolved);
                }

                for repo_path in project.manifest.iter_repo_paths() {
                    known_repos.insert(repo_path.clone());
                }
                projects.push(project);
            }
            Err(e) => {
                // The whole chain, not either end of it. Loading a project
                // reads two files with two different remedies, and the
                // layer naming which one failed is minted by the loader
                // that failed — so an outermost-only or innermost-only
                // render drops either the remedy or the file it applies to.
                let cause = format!("{e:#}");
                unparseable_projects.push((name.clone(), manifest_path, cause));
            }
        }
    }

    // Orphan detection is weave-wide by construction: a repo is orphaned only
    // if it belongs to *no* project, which a partial load cannot establish.
    let check_orphans = scope_all || active_project.is_none();

    let input = CheckInput {
        known_repos,
        repos_on_disk: session.repos_on_disk().to_vec(),
        projects,
        head_revisions,
        resolved_locks,
        check_orphans,
    };

    Ok(DoctorWorld {
        workspace_dir,
        session,
        vcs,
        active_project,
        scope_all,
        input,
        unparseable_projects,
        head_read_failures,
        lock_resolve_failures,
        projects_dir_read_error,
        project_scan,
    })
}

/// Whether the drift scan announces its progress on stderr.
///
/// A workspace-scale run (80+ workweaves × ~13 repos) is silent for many
/// seconds otherwise, and the operator cannot tell it apart from a hang. The
/// wire format stays silent: stdout carries the document and a caller piping
/// it has no report to read the heartbeat against.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanProgress {
    Silent,
    Heartbeat,
}

/// What one pass of [`collect_doctor_violations`] found.
///
/// `repo_locations` resolves a `(workweave, repo)` pair from a violation back
/// to the worktree it was observed in, which is what the text renderer's
/// `--fix` arms need to act on a finding they did not scan for themselves.
struct DoctorFindings {
    violations: Vec<CheckViolation>,
    workweave_dirs: std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
    repo_locations:
        std::collections::HashMap<(Option<WorkweaveName>, RepoPath), std::path::PathBuf>,
}

/// The one place `rwv doctor` decides which scans run.
///
/// Both renderers consume what this returns — the text report in
/// [`run_check`] and the wire format in [`run_check_json`] — so a scan
/// reachable from one of them is reachable from the other by construction.
/// Deciding twice is what let a whole finding kind reach the text report and
/// never the JSON one.
///
/// Repairs are not collection: `--fix` runs its bulk arms before this and its
/// per-finding arms over the vector this returns, so a scan here always
/// observes the state the operator is left in. The merge-driver-config probe
/// relies on that ordering rather than on a `fix` flag of its own — under
/// `--fix` the plant has already run, so the probe sees the config present.
///
/// Findings an integration raised are outside this vector; they are `Issue`s,
/// and [`collect_doctor_issues`] is the pass both renderers take them from.
fn collect_doctor_violations(
    ctx: &crate::workspace::WorkspaceContext,
    world: &DoctorWorld,
    progress: ScanProgress,
) -> DoctorFindings {
    use crate::workspace::Checkout;

    let workspace_dir = &world.workspace_dir;
    let input = &world.input;
    let vcs = world.vcs.as_ref();
    let project_scope = world.project_scope();

    let mut violations = find_violations(input);

    {
        use crate::workspace::read_active_project;
        if let Some(active_name) = read_active_project(ctx.primary_path()) {
            let project_dir = project_dir(ctx.primary_path(), active_name.as_str());
            if !project_dir.is_dir() {
                violations.push(CheckViolation::DanglingActiveProject {
                    project: active_name,
                    missing_dir: project_dir,
                });
            }
        }
    }

    violations.extend(scan_weave_root_identity(
        ctx.primary_path(),
        ctx.active_path(),
    ));

    for (project, manifest_path, message) in &world.unparseable_projects {
        violations.push(CheckViolation::UnparseableProject {
            project: project.clone(),
            manifest_path: manifest_path.clone(),
            message: message.clone(),
        });
    }

    // Found by walking the directory rather than read off a `Project`: a
    // project whose only manifest is the legacy one never loads.
    for finding in scan_workspace_for_legacy_manifests(workspace_dir) {
        if let Some(active) = project_scope {
            if finding.project.as_str() != active {
                continue;
            }
        }
        violations.push(CheckViolation::LegacyManifestFormat {
            project: finding.project,
            legacy_path: finding.legacy_path,
        });
    }

    for finding in scan_for_legacy_workweave_markers(ctx.primary_path()) {
        violations.push(CheckViolation::LegacyWorkweaveMarker {
            marker_path: finding.marker_path,
            primary: finding.primary,
        });
    }

    for project in crate::workspace::discover_projects(ctx.primary_path()) {
        if let Some(active) = project_scope {
            if project.as_str() != active {
                continue;
            }
        }
        match crate::workspace::pending_index_migration(ctx.primary_path(), &project) {
            Ok(Some(index_path)) => violations.push(CheckViolation::LegacyWorkweaveIndex {
                project,
                index_path,
            }),
            Ok(None) => {}
            // An index that does not parse has no shape to classify, and this
            // loop is `project_scope`-narrowed where the read that suppresses
            // the findings a parse failure invalidates is not. Reporting it
            // here too would let a narrowed scan suppress and stay silent, so
            // `scan_registry_reconciliation` owns the whole state.
            Err(_) => {}
        }
    }

    violations.extend(scan_workweave_tree_integrity(vcs, ctx.primary_path()));
    violations.extend(scan_confusable_siblings(
        ctx.primary_path(),
        &input.projects,
    ));
    violations.extend(scan_provenance(workspace_dir, &input.projects));
    violations.extend(scan_phantom_merge_drivers(workspace_dir, &input.projects));

    // Warning severity throughout, so `--json` reports them while the default
    // exit status stays 0. The scan needs an `IntegrationContext`, which is
    // why it is not part of `find_violations`.
    {
        use crate::integration::Integration;
        let builtin = crate::integrations::builtin_integrations();
        let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
        let cargo = crate::integrations::CargoWorkspace;
        for project in &input.projects {
            let detection_cache = crate::integration_runner::build_detection_cache(
                &integrations,
                workspace_dir,
                project.manifest.iter_entries(),
            );
            let ctx_base = world.session.context_base(
                &project.name,
                &detection_cache,
                project.manifest.workweave.as_ref(),
            );
            let default_cfg = crate::manifest::IntegrationConfig::default();
            let cargo_cfg = project
                .manifest
                .integrations
                .get(cargo.name())
                .unwrap_or(&default_cfg);
            let cargo_ctx = ctx_base.build_context(cargo_cfg, &project.manifest);
            if let Ok(vs) = scan_cargo_ecosystem(&cargo_ctx) {
                violations.extend(vs);
            }
        }
    }

    // `git worktree add` does not init submodules, so a workweave forked from
    // a repo with submodules carries empty submodule dirs when the create-time
    // init failed. Reachable only from primary, which is where the workweave
    // dirs are enumerated.
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        violations.extend(scan_uninitialized_submodules_in_workweaves(
            ctx.primary_path(),
            &input.projects,
        ));
    }

    violations.extend(scan_clone_topology(
        vcs,
        ctx.primary_path(),
        &input.known_repos,
    ));

    // Both receipt scans read the primary's `.rwv-workweave-index` and the one
    // physical refdb every linked worktree shares. There is no per-weave copy,
    // so which weave invoked doctor does not enter into it.
    scan_dangling_receipts(vcs, ctx.primary_path(), project_scope, &mut violations);
    scan_pre_flat_receipts(ctx.primary_path(), project_scope, &mut violations);

    for v in scan_branch_discipline(ctx.primary_path(), vcs, &input.projects) {
        if let Some(active) = project_scope {
            if !branch_discipline_in_scope(&v, ctx.primary_path(), active, &input.known_repos) {
                continue;
            }
        }
        violations.push(v);
    }

    // Index-drift and working-tree-drift over every materialized worktree a
    // manifest names, plus — from primary — each workweave's copy. Deduped by
    // `(workweave, abs)` so repos shared between projects cost one round of
    // git subprocesses, and classified in a single pass by `classify_drift`.
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
            let Ok(ww_name) = WorkweaveName::new(ww_name_str) else {
                continue;
            };
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

    let total_scans = index_scan.len();
    let progress_every = total_scans.div_ceil(20).max(1);
    if progress == ScanProgress::Heartbeat && total_scans > 0 {
        eprintln!("doctor: scanning {total_scans} worktree(s) for drift...");
    }

    for (i, (ww_label, repo_abs, repo_path)) in index_scan.iter().enumerate() {
        if progress == ScanProgress::Heartbeat && total_scans >= 50 && (i + 1) % progress_every == 0
        {
            eprintln!("doctor: scanned {}/{total_scans}", i + 1);
        }
        // A linked worktree whose canonical store was removed out-of-band
        // fails every git command in it; classifying first would misreport
        // that as live edits. Only workweave entries can have one — a primary
        // entry IS the canonical store.
        if let Some(ww_name) = ww_label {
            if let Some(canonical_path) = worktree_canonical_clone_missing(repo_abs) {
                violations.push(CheckViolation::MissingCanonicalClone {
                    workweave: ww_name.clone(),
                    repo: repo_path.clone(),
                    canonical_path,
                });
                continue;
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

    // State hygiene widens the drift enumeration by the project repos:
    // `projects/<name>/` is itself a git repo and carries savepoints, but is
    // not a `repositories:` entry so the manifest walk above misses it.
    let mut hygiene_targets: Vec<StateHygieneScanTarget> = index_scan
        .iter()
        .map(|(ww, abs, repo)| StateHygieneScanTarget {
            workweave: ww.clone(),
            abs: abs.clone(),
            repo: repo.clone(),
        })
        .collect();
    let mut hygiene_seen: std::collections::HashSet<(Option<WorkweaveName>, std::path::PathBuf)> =
        index_scan
            .iter()
            .map(|(ww, abs, _)| (ww.clone(), abs.clone()))
            .collect();
    for project in &input.projects {
        let project_rel = project_rel_path(project.name.as_str());
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
        for (ww_name, ww_dir) in workweave_dirs.iter() {
            let ww_project_abs = ww_dir.join(&project_rel);
            if ww_project_abs.is_dir()
                && hygiene_seen.insert((Some(ww_name.clone()), ww_project_abs.clone()))
            {
                hygiene_targets.push(StateHygieneScanTarget {
                    workweave: Some(ww_name.clone()),
                    abs: ww_project_abs,
                    repo: project_repo_path.clone(),
                });
            }
        }
    }
    drop(hygiene_seen);

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

    violations.extend(scan_state_hygiene(
        vcs,
        &hygiene_targets,
        &hygiene_op_state_targets,
    ));

    for project in &input.projects {
        let project_repo = project_dir(workspace_dir, project.name.as_str());
        if !project_repo.is_dir() {
            continue;
        }
        match vcs.replay_exclusion_state(&project_repo, std::path::Path::new(LockFile::FILE_NAME)) {
            Ok(state) => {
                if let Some(sub_kind) = replay_exclusion_finding(state) {
                    violations.push(CheckViolation::MissingReplayExclusion {
                        project: project.name.clone(),
                        sub_kind,
                    });
                }
            }
            Err(e) => violations.push(CheckViolation::ReplayExclusionUnreadable {
                project: project.name.clone(),
                error: e.to_string(),
            }),
        }
        match crate::git::has_rwv_merge_driver_config(&project_repo) {
            Ok(true) => {}
            Ok(false) => violations.push(CheckViolation::MissingMergeDriverConfig {
                project: project.name.clone(),
                config_key: crate::git::RWV_MERGE_DRIVER_CONFIG_KEY.to_owned(),
            }),
            Err(e) => violations.push(CheckViolation::MergeDriverConfigUnreadable {
                project: project.name.clone(),
                config_key: crate::git::RWV_MERGE_DRIVER_CONFIG_KEY.to_owned(),
                error: e.to_string(),
            }),
        }
    }

    // Doctor is the diagnostic of last resort; a repo whose HEAD would not
    // read, or a lock entry the local clone has never seen, is exactly the
    // wrong signal to swallow.
    for (repo, error) in &world.head_read_failures {
        violations.push(CheckViolation::HeadUnreadable {
            repo: repo.clone(),
            error: error.clone(),
        });
    }
    for (project, repo) in &world.lock_resolve_failures {
        violations.push(CheckViolation::UnresolvableLockEntry {
            project: project.clone(),
            repo: repo.clone(),
        });
    }
    if let Some((path, error)) = &world.projects_dir_read_error {
        violations.push(CheckViolation::ProjectsDirUnreadable {
            path: path.clone(),
            error: error.clone(),
        });
    }
    for dir in &world.project_scan.projectless {
        violations.push(CheckViolation::ProjectlessDir { dir: dir.clone() });
    }
    for unnameable in &world.project_scan.unnameable {
        violations.push(CheckViolation::UnnameableProject {
            dir: unnameable.dir.clone(),
            derived: unnameable.derived.clone(),
            error: unnameable.error.to_string(),
        });
    }

    let repo_locations = hygiene_targets
        .into_iter()
        .map(|t| ((t.workweave, t.repo), t.abs))
        .collect();

    DoctorFindings {
        violations,
        workweave_dirs,
        repo_locations,
    }
}

/// Run `rwv doctor --json`.
///
/// Emits `{ "$schema": "...", "violations": [...], "issues": [...] }` to
/// stdout, from the same two collection passes [`run_check`] renders as text.
///
/// Returns `Ok(true)` — the caller's exit-1 signal — when any violation was
/// found or any integration issue was an error. The violation half of that is
/// wider than [`run_check`]'s, which exits non-zero on errors alone, and stays
/// so: a caller already scripting against `--json` reads a warning-severity
/// violation as exit 1 today.
///
/// There is no `--fix` on this path, so nothing here repairs and nothing mints
/// a `--fix` failure.
///
/// When `scope_all` is `false` (the default), only the active project is
/// checked and orphan detection is skipped. Pass `scope_all = true` (`--all`)
/// to reproduce the weave-wide scan.
pub fn run_check_json(
    ctx: &crate::workspace::WorkspaceContext,
    scope_all: bool,
    kind_filter: Option<&KindFilter>,
) -> anyhow::Result<bool> {
    // The P2 gate, same as the text path. This surface never RECORDS a
    // floor — `--json` is a machine-reading surface and does not mutate
    // weave state; the floor records from the operator's text-mode run.
    crate::health_floor::enforce(ctx.primary_path())?;
    let world = load_doctor_world(ctx, scope_all)?;
    let DoctorFindings {
        violations,
        workweave_dirs,
        ..
    } = collect_doctor_violations(ctx, &world, ScanProgress::Silent);
    // Filtering selects a SUBSET of the records — each surviving record is
    // byte-identical to its unfiltered self, so consumers reading
    // `violations[]` (the per-class baseline capture among them) see the
    // same shapes whether or not a filter is active. Integration issues
    // are not violations of any kind and are absent from a filtered view.
    let violations: Vec<CheckViolation> = match kind_filter {
        Some(filter) => violations
            .into_iter()
            .filter(|v| filter.admits(v))
            .collect(),
        None => violations,
    };
    let issues = if kind_filter.is_none() {
        collect_doctor_issues(&world, Repair::Report)
    } else {
        Vec::new()
    };
    let has_violations = !violations.is_empty()
        || issues
            .iter()
            .any(|i| i.severity == crate::integration::Severity::Error);
    // Discover `rwv-*` executables on PATH for the inventory. This is
    // reporting only — the presence or absence of plugins never affects the
    // has_violations signal or the doctor exit code.
    let plugins = crate::plugins::discover_plugins(None::<&std::ffi::OsStr>);
    let advisories = stale_generation_findings(&world)
        .iter()
        .map(stale_generation_advisory)
        .collect();
    let payload = build_doctor_json(
        violations,
        issues,
        &world.workspace_dir,
        &workweave_dirs,
        ctx.resolution(),
        plugins,
        advisories,
    );
    let out =
        serde_json::to_string_pretty(&payload).context("failed to serialize doctor output")?;
    println!("{out}");
    Ok(has_violations)
}
