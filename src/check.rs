//! Convention checks: orphaned clones, dangling refs, pins that disagree with their checkouts, index drift, working-tree drift, etc.
//!
//! `rwv doctor` builds a workspace-wide inventory from all projects, then runs
//! a series of checks. Integration check hooks are run separately.

use crate::git::RWV_MERGE_DRIVER_PREFIX;
use crate::integration::{IntegrationContextBase, Issue, IssueKind, IssueSource};
use crate::manifest::{
    LockFile, Manifest, Project, ProjectName, RepoPath, Role, WorkweaveId, WorkweaveName,
};
use crate::op_state::SavepointKey;
use crate::path_spelling::{CanonicalPath, WirePath};
use crate::refusal::{refusal, RefusalKind};
use crate::vcs::{
    CommitId, Holds, PathStanding, RawRevisionId, RefreshScope, RefreshWrite, ResolvedRevisionId,
    VcsError,
};
use crate::workspace::{project_dir, project_rel_path, projects_dir, PointerContent, Resolution};
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
#[derive(Debug, Clone)]
pub enum CheckViolation {
    /// A directory under a registry path not listed in any project's `rwv.toml`.
    OrphanedClone { path: RepoPath },

    /// A VCS repo under a top-level directory that is neither a built-in
    /// registry segment nor named by any project manifest under the weave
    /// root — so it is not just unregistered, it sits somewhere
    /// [`crate::workspace::scan_repos_on_disk`] never looks, unlike
    /// [`OrphanedClone`](Self::OrphanedClone), which the same walk does
    /// reach.
    UnknownSegmentClone { path: RepoPath },

    /// An `rwv.toml` entry pointing to a path not present on disk.
    DanglingReference {
        project: ProjectName,
        repo: RepoPath,
        /// The path the entry's URL places it at, when a clone of that remote
        /// stands there and no entry of the project names the path: the
        /// entry's checkout, moved to its placement ahead of the re-key.
        clone_at_placement: Option<RepoPath>,
    },

    /// A member checkout whose tracked regular files do not hold the
    /// permissions its role asks for: writable in a `reference` member, or
    /// read-only in any other. `files` counts them.
    MemberPermissions {
        /// `None` for the primary weave.
        workweave: Option<WorkweaveId>,
        repo: RepoPath,
        kind: PermissionsKind,
        files: usize,
    },

    /// A canonical clone of the primary that some projects listing it declare
    /// `reference` and others do not. Its files hold the role other than
    /// `reference`, so the projects declaring `reference` get a writable clone.
    RoleDisagreement {
        repo: RepoPath,
        /// Every project listing the clone, with the role it declares.
        roles: Vec<(ProjectName, Role)>,
    },

    /// A member two projects list, whose live weave branches record
    /// different `history` for it.
    HistoryDisagreement {
        repo: RepoPath,
        /// Every live weave branch recording a `history` for the member.
        histories: Vec<crate::alignment::RecordedHistory>,
    },

    /// A member's checkout does not stand at the commit a project's lock pins
    /// for it, or the lock pins it at no commit that can be stood at.
    ///
    /// Raised for an entry the *resolved* lock carries, for a manifest member
    /// the raw lock has no entry for, and for an entry the clone cannot
    /// resolve. An entry whose repo is absent from disk is dropped before the
    /// scan runs and is reported by [`run_check_locked`] instead; both
    /// surfaces name the same two revisions in the same spelling for every
    /// entry both of them see.
    Pin {
        project: ProjectName,
        repo: RepoPath,
        sub_kind: PinKind,
        /// The commit the lock pins, for the sub-kinds that compare it with
        /// the checkout's.
        locked: Option<ResolvedRevisionId>,
        /// The commit the checkout is at, for the same sub-kinds.
        actual: Option<ResolvedRevisionId>,
        /// The commits only the checkout holds, for `unpinned`.
        listed: Vec<String>,
    },

    /// A workweave without the worktree a member its `rwv.lock` pins needs —
    /// nothing at the slot, or a symlink there — or, for [`DriftKind::Extra`],
    /// a checkout the lock still names and the manifest no longer does.
    WorkweaveDrift {
        /// `None` for the canonical clone in the primary weave, which only
        /// [`DriftKind::Extra`] reports.
        workweave: Option<WorkweaveId>,
        kind: DriftKind,
        repo: RepoPath,
    },

    /// What the VCS records for a repo's paths does not match its HEAD tree: stale entries a
    /// shared-ref advance from a sibling worktree left, or staged changes.
    RecordDrift {
        /// `None` for repos in the primary weave.
        workweave: Option<WorkweaveId>,
        repo: RepoPath,
        kind: RecordDriftKind,
    },

    /// A git repo's working-tree files do not match its HEAD tree: stale files
    /// a shared-ref advance from a sibling worktree left, or edits.
    WorkingTreeDrift {
        workweave: Option<WorkweaveId>,
        repo: RepoPath,
        kind: WorkingTreeDriftKind,
    },

    /// A project repo's lock merge driver is not set up as this release sets
    /// it up. Every verb that writes the project repo or its lock refuses the
    /// same state.
    MissingLockMergeDriver {
        project: ProjectName,
        sub_kind: MissingLockMergeDriverKind,
    },

    /// Reading a project repo's lock merge driver setup failed, so it is
    /// neither confirmed nor reported missing.
    LockMergeDriverUnreadable { project: ProjectName, error: String },

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

    /// An `rwv.lock` entry that names a tag or branch where a commit id
    /// belongs. It resolves to wherever the name points now, so it pins
    /// nothing. Report-only: `rwv lock` rewrites the entry.
    LockPinnedByName {
        project: ProjectName,
        repo: RepoPath,
        version: RawRevisionId,
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

    /// The primary's `.rwv-active` is empty, unreadable, or holds text that is
    /// not a project name. A verb that falls through to the pointer refuses on
    /// it. Auto-fixable: the file selects nothing, so removing it loses
    /// nothing.
    UnreadableActiveProject {
        /// The `.rwv-active` file.
        path: PathBuf,
        /// What is wrong with what it holds.
        detail: String,
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
        sub_kind: WeaveRootIdentityConflictKind<PathBuf>,
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

    /// A workweave index still at `projects/<project>/.rwv-workweave-index`,
    /// where earlier releases kept it, in any shape.
    ///
    /// Auto-fixable: `--fix` converts it and moves it to the project clone's
    /// shared directory. Every verb that reads the index refuses until then.
    ///
    /// Reported rather than refused at read (unlike the marker), because the
    /// conversion has to be able to read the file it is about to move.
    LegacyWorkweaveIndex {
        /// The project whose index is at the old path.
        project: ProjectName,
        /// Absolute path to the old-path index file.
        index_path: PathBuf,
        /// How the old file was written.
        sub_kind: LegacyWorkweaveIndexKind,
    },

    /// A workweave index that exists but does not parse, at the path it lives
    /// at or at the old path, so nothing derived from it is evaluated: the
    /// recorded placements and the conversion
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
        /// Whether `project` is the project the scanned root presents.
        /// `rwv materialize` takes no project argument — it acts on
        /// whichever project that is — so the remedy differs when it is not.
        active: bool,
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
    /// project rwv cannot see into. The remedy differs between the two files,
    /// so `message` carries the one the failing loader minted rather than this
    /// variant naming either.
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
        /// Whether the file failed for keys an earlier release wrote.
        sub_kind: UnparseableProjectKind,
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
        sub_kind: WorkweaveTreeIntegrityKind<PathBuf>,
    },

    /// A provenance violation: where a member's url, remote or path disagrees
    /// with what the manifest says of it.
    ///
    /// The `origin-url-mismatch` case needs the operator to decide whether
    /// the manifest or the remote is the source of truth; reference-role
    /// repos may intentionally diverge.
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
    /// the physical object-store topology).
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
        sub_kind: CloneTopologyKind<PathBuf>,
    },
    /// A member's checkout is not where the checked-out branch of its
    /// workspace says it should be.
    Alignment {
        /// Absolute path to the checkout.
        repo_path: PathBuf,
        /// The project whose workspace the checkout belongs to.
        project: ProjectName,
        sub_kind: AlignmentKind,
        /// The branch the checkout is on, where it is on one.
        branch: Option<String>,
        /// The commits listed beneath the state: those only the checkout holds.
        listed: Vec<String>,
    },
    /// A worktree registration in a repo that no longer leads to a
    /// worktree: its directory is gone, or stands without its link back to
    /// the repo. [`RegistrationDirectoryKind`] says which.
    StaleWorktreeRegistration {
        /// The workweave the *registering* repo (the one that holds the
        /// stale `.git/worktrees/` entry) lives in; `None` when it is in the
        /// primary weave.
        workweave: Option<WorkweaveId>,
        /// The registering repo's manifest-relative path.
        repo: RepoPath,
        /// Absolute path of the registered worktree, as recorded in the
        /// VCS's worktree list.
        worktree_path: PathBuf,
        sub_kind: RegistrationDirectoryKind,
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

    /// A `.rwv-op` file this rwv cannot read: not even the op it names
    /// parses, or another release's schema wrote it. Every verb that reads it
    /// refuses with the same token. While one stands no savepoint whose op it
    /// may name is reported as orphaned, so `--fix` drops none of them.
    UnreadableOpRecord {
        /// Absolute path to the workspace dir that holds the `.rwv-op` file.
        workspace_dir: PathBuf,
        /// The refusal a verb reading it prints, naming the verb that ends
        /// the operation.
        error: String,
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

    /// A landing claim whose owner workspace is not here to read: on another
    /// mount, outside a sandbox, or deleted. Whether its op still stands
    /// cannot be told, so no landing takes it over; `--fix` releases it, the
    /// operator's word that the op is gone.
    ClaimOwnerUnreachable {
        /// The directory holding the claim file.
        claim_dir: PathBuf,
        /// Op id the claim names.
        op_id: String,
        /// Owner workspace the claim names.
        recorded_owner: PathBuf,
    },

    /// A landing claim, or a takeover guard beside one, holding bytes this rwv
    /// does not read: an rwv of another release wrote it, or it is corrupt.
    /// Never judged dead, so no landing takes it over; `--fix
    /// --release-unreadable-claims` releases it, the operator's word that no
    /// operation of another release holds it.
    ClaimUnreadable {
        file: PathBuf,
        /// The bytes it held when it was reported; the release removes it only
        /// while it still holds them.
        seen: Vec<u8>,
    },

    /// A landing claim, or a takeover guard beside one, that this rwv cannot
    /// read at all: a file this user may not read, or something that is not a
    /// file. Whose it is cannot be told and its bytes cannot be compared, so
    /// nothing in rwv releases it.
    ClaimUnopenable {
        file: PathBuf,
        /// Rendered read error.
        error: String,
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
    /// the last pointer to discarded work; `--fix` converts it to a backup).
    OrphanedSavepoint {
        /// The workweave the holding repo lives in; `None` for the primary.
        workweave: Option<WorkweaveId>,
        /// The holding repo's manifest-relative path.
        repo: RepoPath,
        /// The key read from the savepoint ref's trailing path component.
        key: SavepointKey,
        /// Safe-vs-live classification.
        sub_kind: OrphanedSavepointKind,
    },

    /// A ref under the savepoint namespace that no op id names. rwv filed
    /// none of it: a hand-made ref, a damaged name, or one written by a
    /// different rwv. What it points at is unknown, so it is reported and
    /// never dropped.
    UnrecognizedSavepointRef {
        /// The workweave the holding repo lives in; `None` for the primary.
        workweave: Option<WorkweaveId>,
        /// The holding repo's manifest-relative path.
        repo: RepoPath,
        /// The holding repo's directory on disk.
        holder: PathBuf,
        /// The reference, spelled as the VCS spells it.
        ref_name: String,
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
    /// particular, `git diff-index HEAD` fails and the drift reading would
    /// misattribute the failure as `LiveEdits`. This variant surfaces the true
    /// root cause instead.
    ///
    /// Repair: `rwv update --project <p>` at the primary weave clones it again,
    /// and `rwv doctor --fix` there relinks this worktree. No auto-fix of the
    /// clone itself — doctor never clones (network stays behind explicit verbs).
    MissingCanonicalClone {
        /// This finding only fires for workweave worktrees, never for
        /// primary-weave repos.
        workweave: WorkweaveId,
        /// Manifest-relative path to the affected repo.
        repo: RepoPath,
        /// Absolute path of the canonical clone directory that is missing.
        canonical_path: PathBuf,
    },

    /// A member checkout in a workweave whose manifest entry does not opt
    /// out with `recurse-submodules = false` has a `.gitmodules` file, and one or
    /// more of its listed submodule paths that git does not decline are
    /// empty directories (or absent).
    ///
    /// Warning severity. Detection reads only local data. The paths are
    /// split by [`SubmoduleCommitKind`] into one finding per sub-kind.
    UninitializedSubmodule {
        workweave: WorkweaveId,
        /// Manifest-relative path to the repo that has uninitialized submodules.
        repo: RepoPath,
        sub_kind: SubmoduleCommitKind,
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
            | CheckViolation::UnknownSegmentClone { .. }
            | CheckViolation::DanglingReference { .. }
            | CheckViolation::RoleDisagreement { .. }
            | CheckViolation::HistoryDisagreement { .. }
            | CheckViolation::LegacyManifestFormat { .. }
            | CheckViolation::StaleOpState { .. }
            | CheckViolation::UnreadableOpRecord { .. }
            | CheckViolation::CargoVersionSkew { .. }
            | CheckViolation::CargoPatchShadowing { .. }
            | CheckViolation::MissingCanonicalClone { .. }
            | CheckViolation::LockMergeDriverUnreadable { .. }
            | CheckViolation::HeadUnreadable { .. }
            | CheckViolation::ProjectsDirUnreadable { .. }
            | CheckViolation::ProjectlessDir { .. }
            | CheckViolation::UnnameableProject { .. }
            | CheckViolation::LockPinnedByName { .. }
            | CheckViolation::UnreadableOwnedState { .. }
            | CheckViolation::PhantomMergeDriver { .. } => ReportOnly,

            CheckViolation::DanglingActiveProject { .. }
            | CheckViolation::UnreadableWorkweaveIndex { .. }
            | CheckViolation::UnreadableActiveProject { .. }
            | CheckViolation::LegacyWorkweaveMarker { .. }
            | CheckViolation::DeadOpLease { .. }
            | CheckViolation::ClaimOwnerUnreachable { .. }
            | CheckViolation::MemberPermissions { .. } => Auto,

            CheckViolation::ClaimUnreadable { .. } => Consented("--release-unreadable-claims"),
            CheckViolation::ClaimUnopenable { .. } => ReportOnly,

            CheckViolation::UnparseableProject { sub_kind, .. } => sub_kind.fix_disposition(),
            CheckViolation::Pin { sub_kind, .. } => sub_kind.fix_disposition(),
            CheckViolation::LegacyWorkweaveIndex { sub_kind, .. } => sub_kind.fix_disposition(),
            CheckViolation::MissingLockMergeDriver { sub_kind, .. } => sub_kind.fix_disposition(),
            CheckViolation::UninitializedSubmodule { sub_kind, .. } => match sub_kind {
                SubmoduleCommitKind::Fetched => Auto,
                SubmoduleCommitKind::Unfetched => ReportOnly,
            },
            CheckViolation::StaleWorktreeRegistration { sub_kind, .. } => match sub_kind {
                RegistrationDirectoryKind::DirectoryGone => Auto,
                RegistrationDirectoryKind::DirectoryStands => ReportOnly,
            },
            CheckViolation::WorkweaveDrift { kind, .. } => match kind {
                DriftKind::Missing | DriftKind::Symlink | DriftKind::DanglingLink => Auto,
                DriftKind::MissingUnfetched | DriftKind::SymlinkUnfetched | DriftKind::Extra => {
                    ReportOnly
                }
                DriftKind::DanglingLinkUnfetched => Consented("--abandon-lost-pins"),
            },
            CheckViolation::RecordDrift { kind, .. } => match kind {
                RecordDriftKind::SafeToFix => Auto,
                RecordDriftKind::LiveRecorded => ReportOnly,
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
                | WorkweaveTreeIntegrityKind::UnreadableMarker { .. }
                | WorkweaveTreeIntegrityKind::NestedWorkweaveDir { .. }
                | WorkweaveTreeIntegrityKind::MisnamedDir { .. } => ReportOnly,
            },
            CheckViolation::Provenance { sub_kind, .. } => match sub_kind {
                ProvenanceKind::OriginUrlMismatch { .. }
                | ProvenanceKind::RelativeRemote { .. }
                | ProvenanceKind::UnresolvableUrl { .. }
                | ProvenanceKind::PlacementDisagreement { .. } => ReportOnly,
            },
            CheckViolation::CloneTopology { sub_kind, .. } => match sub_kind {
                CloneTopologyKind::StandaloneInWorkweave { .. } => Auto,
                CloneTopologyKind::DisconnectedWeaveClone { .. }
                | CloneTopologyKind::WrongParentWorktree { .. }
                | CloneTopologyKind::WeaveCloneIsWorktree { .. } => ReportOnly,
            },
            CheckViolation::Alignment { sub_kind, .. } => sub_kind.fix_disposition(),
            CheckViolation::OrphanedSavepoint { sub_kind, .. } => match sub_kind {
                OrphanedSavepointKind::Redundant | OrphanedSavepointKind::Live => Auto,
            },
            CheckViolation::UnrecognizedSavepointRef { .. } => ReportOnly,
            CheckViolation::ConfusableSiblings { .. } => ReportOnly,
        }
    }

    /// The severity this violation carries toward `rwv doctor`'s exit code
    /// and toward the `Issue` it renders as in text output.
    ///
    /// Mirrors [`fix_disposition`](Self::fix_disposition) in shape: the one
    /// place the per-(kind, sub-kind) severity table lives, matched
    /// exhaustively down to sub-kind so a finding added without an entry
    /// here does not compile. `--json`'s `violations` array carries no
    /// severity field of its own — `docs/reference/doctor-findings.md`
    /// states the same table for a reader without this method.
    pub fn severity(&self) -> crate::integration::Severity {
        use crate::integration::Severity::{Error, Warning};
        match self {
            CheckViolation::OrphanedClone { .. }
            | CheckViolation::UnknownSegmentClone { .. }
            | CheckViolation::DanglingReference { .. }
            | CheckViolation::HeadUnreadable { .. }
            | CheckViolation::ProjectsDirUnreadable { .. }
            | CheckViolation::LegacyManifestFormat { .. }
            | CheckViolation::DanglingActiveProject { .. }
            | CheckViolation::UnreadableActiveProject { .. }
            | CheckViolation::WeaveRootIdentityConflict { .. }
            | CheckViolation::UnreadableWorkweaveIndex { .. }
            | CheckViolation::CloneTopology { .. } => Error,

            CheckViolation::MemberPermissions { .. }
            | CheckViolation::RoleDisagreement { .. }
            | CheckViolation::HistoryDisagreement { .. }
            | CheckViolation::RecordDrift { .. }
            | CheckViolation::WorkingTreeDrift { .. }
            | CheckViolation::MissingLockMergeDriver { .. }
            | CheckViolation::LockMergeDriverUnreadable { .. }
            | CheckViolation::ProjectlessDir { .. }
            | CheckViolation::UnnameableProject { .. }
            | CheckViolation::LockPinnedByName { .. }
            | CheckViolation::LegacyWorkweaveMarker { .. }
            | CheckViolation::UnreadableOwnedState { .. }
            | CheckViolation::WorkweaveTreeIntegrity { .. }
            | CheckViolation::Alignment { .. }
            | CheckViolation::StaleWorktreeRegistration { .. }
            | CheckViolation::StaleOpState { .. }
            | CheckViolation::UnreadableOpRecord { .. }
            | CheckViolation::DeadOpLease { .. }
            | CheckViolation::ClaimOwnerUnreachable { .. }
            | CheckViolation::ClaimUnreadable { .. }
            | CheckViolation::ClaimUnopenable { .. }
            | CheckViolation::OrphanedSavepoint { .. }
            | CheckViolation::UnrecognizedSavepointRef { .. }
            | CheckViolation::ConfusableSiblings { .. }
            | CheckViolation::CargoVersionSkew { .. }
            | CheckViolation::CargoPatchShadowing { .. }
            | CheckViolation::MissingCanonicalClone { .. }
            | CheckViolation::UninitializedSubmodule { .. }
            | CheckViolation::PhantomMergeDriver { .. } => Warning,

            CheckViolation::UnparseableProject { sub_kind, .. } => sub_kind.severity(),
            CheckViolation::Pin { sub_kind, .. } => sub_kind.severity(),
            CheckViolation::LegacyWorkweaveIndex { sub_kind, .. } => sub_kind.severity(),
            CheckViolation::WorkweaveDrift { kind, .. } => match kind {
                DriftKind::Extra => Warning,
                DriftKind::Missing
                | DriftKind::MissingUnfetched
                | DriftKind::Symlink
                | DriftKind::SymlinkUnfetched
                | DriftKind::DanglingLink
                | DriftKind::DanglingLinkUnfetched => Error,
            },
            CheckViolation::Provenance { sub_kind, .. } => match sub_kind {
                ProvenanceKind::OriginUrlMismatch { .. }
                | ProvenanceKind::RelativeRemote { .. }
                | ProvenanceKind::PlacementDisagreement { .. } => Warning,
                ProvenanceKind::UnresolvableUrl { .. } => Error,
            },
        }
    }

    pub fn kind(&self) -> ViolationKind {
        match self {
            CheckViolation::OrphanedClone { .. } => ViolationKind::OrphanedClone,
            CheckViolation::UnknownSegmentClone { .. } => ViolationKind::UnknownSegmentClone,
            CheckViolation::DanglingReference { .. } => ViolationKind::DanglingReference,
            CheckViolation::MemberPermissions { .. } => ViolationKind::MemberPermissions,
            CheckViolation::RoleDisagreement { .. } => ViolationKind::RoleDisagreement,
            CheckViolation::HistoryDisagreement { .. } => ViolationKind::HistoryDisagreement,
            CheckViolation::Pin { .. } => ViolationKind::Pin,
            CheckViolation::WorkweaveDrift { .. } => ViolationKind::WorkweaveDrift,
            CheckViolation::RecordDrift { .. } => ViolationKind::RecordDrift,
            CheckViolation::WorkingTreeDrift { .. } => ViolationKind::WorkingTreeDrift,
            CheckViolation::MissingLockMergeDriver { .. } => ViolationKind::MissingLockMergeDriver,
            CheckViolation::LockMergeDriverUnreadable { .. } => {
                ViolationKind::LockMergeDriverUnreadable
            }
            CheckViolation::HeadUnreadable { .. } => ViolationKind::HeadUnreadable,
            CheckViolation::ProjectsDirUnreadable { .. } => ViolationKind::ProjectsDirUnreadable,
            CheckViolation::ProjectlessDir { .. } => ViolationKind::ProjectlessDir,
            CheckViolation::UnnameableProject { .. } => ViolationKind::UnnameableProject,
            CheckViolation::LockPinnedByName { .. } => ViolationKind::LockPinnedByName,
            CheckViolation::LegacyManifestFormat { .. } => ViolationKind::LegacyManifestFormat,
            CheckViolation::DanglingActiveProject { .. } => ViolationKind::DanglingActiveProject,
            CheckViolation::UnreadableActiveProject { .. } => {
                ViolationKind::UnreadableActiveProject
            }
            CheckViolation::WeaveRootIdentityConflict { .. } => {
                ViolationKind::WeaveRootIdentityConflict
            }
            CheckViolation::LegacyWorkweaveMarker { .. } => ViolationKind::LegacyWorkweaveMarker,
            CheckViolation::LegacyWorkweaveIndex { .. } => ViolationKind::LegacyWorkweaveIndex,
            CheckViolation::UnreadableOwnedState { .. } => ViolationKind::UnreadableOwnedState,
            CheckViolation::UnreadableWorkweaveIndex { .. } => {
                ViolationKind::UnreadableWorkweaveIndex
            }
            CheckViolation::UnparseableProject { .. } => ViolationKind::UnparseableProject,
            CheckViolation::WorkweaveTreeIntegrity { .. } => ViolationKind::WorkweaveTreeIntegrity,
            CheckViolation::Provenance { .. } => ViolationKind::Provenance,
            CheckViolation::CloneTopology { .. } => ViolationKind::CloneTopology,
            CheckViolation::Alignment { .. } => ViolationKind::Alignment,
            CheckViolation::StaleWorktreeRegistration { .. } => {
                ViolationKind::StaleWorktreeRegistration
            }
            CheckViolation::StaleOpState { .. } => ViolationKind::StaleOpState,
            CheckViolation::UnreadableOpRecord { .. } => ViolationKind::UnreadableOpRecord,
            CheckViolation::DeadOpLease { .. } => ViolationKind::DeadOpLease,
            CheckViolation::ClaimOwnerUnreachable { .. } => ViolationKind::ClaimOwnerUnreachable,
            CheckViolation::ClaimUnreadable { .. } => ViolationKind::ClaimUnreadable,
            CheckViolation::ClaimUnopenable { .. } => ViolationKind::ClaimUnopenable,
            CheckViolation::OrphanedSavepoint { .. } => ViolationKind::OrphanedSavepoint,
            CheckViolation::UnrecognizedSavepointRef { .. } => {
                ViolationKind::UnrecognizedSavepointRef
            }
            CheckViolation::ConfusableSiblings { .. } => ViolationKind::ConfusableSiblings,
            CheckViolation::CargoVersionSkew { .. } => ViolationKind::CargoVersionSkew,
            CheckViolation::CargoPatchShadowing { .. } => ViolationKind::CargoPatchShadowing,
            CheckViolation::MissingCanonicalClone { .. } => ViolationKind::MissingCanonicalClone,
            CheckViolation::UninitializedSubmodule { .. } => ViolationKind::UninitializedSubmodule,
            CheckViolation::PhantomMergeDriver { .. } => ViolationKind::PhantomMergeDriver,
        }
    }
}

/// Which [`CheckViolation`] variant a finding is, as the `kind` it carries in
/// `rwv doctor --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViolationKind {
    OrphanedClone,
    UnknownSegmentClone,
    DanglingReference,
    MemberPermissions,
    RoleDisagreement,
    HistoryDisagreement,
    Pin,
    WorkweaveDrift,
    RecordDrift,
    WorkingTreeDrift,
    MissingLockMergeDriver,
    LockMergeDriverUnreadable,
    HeadUnreadable,
    ProjectsDirUnreadable,
    ProjectlessDir,
    UnnameableProject,
    LockPinnedByName,
    LegacyManifestFormat,
    DanglingActiveProject,
    UnreadableActiveProject,
    WeaveRootIdentityConflict,
    LegacyWorkweaveMarker,
    LegacyWorkweaveIndex,
    UnreadableOwnedState,
    UnreadableWorkweaveIndex,
    UnparseableProject,
    WorkweaveTreeIntegrity,
    Provenance,
    CloneTopology,
    Alignment,
    StaleWorktreeRegistration,
    StaleOpState,
    UnreadableOpRecord,
    DeadOpLease,
    ClaimOwnerUnreachable,
    ClaimUnreadable,
    ClaimUnopenable,
    OrphanedSavepoint,
    UnrecognizedSavepointRef,
    ConfusableSiblings,
    CargoVersionSkew,
    CargoPatchShadowing,
    MissingCanonicalClone,
    UninitializedSubmodule,
    PhantomMergeDriver,
}

impl ViolationKind {
    pub const ALL: [Self; 45] = [
        Self::OrphanedClone,
        Self::UnknownSegmentClone,
        Self::DanglingReference,
        Self::MemberPermissions,
        Self::RoleDisagreement,
        Self::HistoryDisagreement,
        Self::Pin,
        Self::WorkweaveDrift,
        Self::RecordDrift,
        Self::WorkingTreeDrift,
        Self::MissingLockMergeDriver,
        Self::LockMergeDriverUnreadable,
        Self::HeadUnreadable,
        Self::ProjectsDirUnreadable,
        Self::ProjectlessDir,
        Self::UnnameableProject,
        Self::LockPinnedByName,
        Self::LegacyManifestFormat,
        Self::DanglingActiveProject,
        Self::UnreadableActiveProject,
        Self::WeaveRootIdentityConflict,
        Self::LegacyWorkweaveMarker,
        Self::LegacyWorkweaveIndex,
        Self::UnreadableOwnedState,
        Self::UnreadableWorkweaveIndex,
        Self::UnparseableProject,
        Self::WorkweaveTreeIntegrity,
        Self::Provenance,
        Self::CloneTopology,
        Self::Alignment,
        Self::StaleWorktreeRegistration,
        Self::StaleOpState,
        Self::UnreadableOpRecord,
        Self::DeadOpLease,
        Self::ClaimOwnerUnreachable,
        Self::ClaimUnreadable,
        Self::ClaimUnopenable,
        Self::OrphanedSavepoint,
        Self::UnrecognizedSavepointRef,
        Self::ConfusableSiblings,
        Self::CargoVersionSkew,
        Self::CargoPatchShadowing,
        Self::MissingCanonicalClone,
        Self::UninitializedSubmodule,
        Self::PhantomMergeDriver,
    ];
}

impl std::fmt::Display for ViolationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::wire_token::wire_token(self))
    }
}

/// Classification of an orphaned savepoint, controlling `--fix` policy.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum OrphanedSavepointKind {
    /// Every tip filed under the key, the checkout's and each branch's, is
    /// reached by a branch, so dropping the savepoints loses no commit.
    Redundant,
    /// Some tip filed under the key is reached by no branch: a savepoint
    /// is the last pointer to it. `--fix` keeps each such tip as a backup,
    /// then drops the key's savepoints.
    Live,
}

/// Why a project directory did not load, controlling `--fix` policy.
#[derive(Debug, Serialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UnparseableProjectKind {
    /// `rwv.toml` names member-entry keys an earlier release wrote. `--fix`
    /// renames them in place and commits only that rewrite.
    PreSchema,
    /// Anything else: a file that is not TOML, a key or value the schema does
    /// not take, or a lock that is not JSON. The file is the operator's to
    /// edit, or the lock's to regenerate.
    Malformed,
}

impl UnparseableProjectKind {
    pub const ALL: [Self; 2] = [Self::PreSchema, Self::Malformed];

    pub fn severity(self) -> crate::integration::Severity {
        match self {
            Self::PreSchema | Self::Malformed => crate::integration::Severity::Error,
        }
    }

    pub fn fix_disposition(self) -> FixDisposition {
        match self {
            Self::PreSchema => FixDisposition::Auto,
            Self::Malformed => FixDisposition::ReportOnly,
        }
    }

    pub(crate) fn of(failure: &anyhow::Error) -> Self {
        if failure
            .chain()
            .any(|link| link.is::<crate::manifest::PreSchemaManifest>())
        {
            Self::PreSchema
        } else {
            Self::Malformed
        }
    }
}

/// How a member's pin stands against its checkout, or whether it has one.
#[derive(Debug, Serialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PinKind {
    /// The lock has a file but no entry for the member.
    None,
    /// The lock pins a commit this clone cannot resolve.
    Unreachable,
    /// The checkout is at an ancestor of the pin.
    Behind,
    /// The checkout holds commits beyond the pin, each pushed or pinned by
    /// another project's live lock.
    Ahead,
    /// The checkout holds commits beyond the pin that nothing else holds.
    Unpinned,
    /// Neither the checkout nor the pin is an ancestor of the other.
    Diverged,
}

impl PinKind {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Unreachable,
        Self::Behind,
        Self::Ahead,
        Self::Unpinned,
        Self::Diverged,
    ];

    pub fn severity(self) -> crate::integration::Severity {
        match self {
            Self::Ahead => crate::integration::Severity::Warning,
            Self::None | Self::Unreachable | Self::Behind | Self::Unpinned | Self::Diverged => {
                crate::integration::Severity::Error
            }
        }
    }

    pub fn fix_disposition(self) -> FixDisposition {
        FixDisposition::ReportOnly
    }

    pub fn entry(self) -> &'static str {
        match self {
            Self::None => {
                "The lock has no entry for a member the manifest lists. Only raised for a \
                 project that has a lock file; a project with no lock yet is a separate, \
                 unlocked state. **What to do:** `rwv member pin <repo> <rev> --commit` gives \
                 the member its pin, or `rwv update --remote --repo <repo> --commit` pins its \
                 upstream's tip. `rwv lock` gives a member no first pin."
            }
            Self::Unreachable => {
                "The lock pins a commit this clone cannot resolve: a lock written against \
                 history that was never fetched here. The entry is kept as a finding rather \
                 than dropped, because dropping it would leave the repo with no pin to \
                 compare against, which reads as healthy. **What to do:** `rwv fetch` brings \
                 in the history that may hold it, or `rwv member pin <repo> <rev>` pins a \
                 reachable commit if the lock is the stale side."
            }
            Self::Behind => {
                "The checkout is at an ancestor of its pin: the lock records commits the \
                 checkout lacks. **What to do:** `rwv update` moves the checkout to the pin, \
                 or `rwv member pin <repo> HEAD` pins the checkout's commit by name, moving \
                 the pin back."
            }
            Self::Ahead => {
                "The checkout holds commits beyond its pin, and each is pushed or pinned by \
                 another project's live lock: the ordinary state between committing and \
                 `rwv lock`, so `rwv doctor` warns and exits as it does for any warning. \
                 **What to do:** `rwv lock` records the checkout's commit."
            }
            Self::Unpinned => {
                "The checkout holds commits beyond its pin that no remote-tracking branch \
                 and no live lock reaches, listed: nothing but this checkout holds them. \
                 **What to do:** `rwv lock --commit` records them."
            }
            Self::Diverged => {
                "Neither the checkout nor its pin is an ancestor of the other. **What to \
                 do:** `rwv member pin <repo> HEAD` pins the checkout's commit by name, \
                 `rwv member pin <repo> <rev>` pins another commit you choose, or `rwv update` \
                 moves the checkout to the pin once its own commits are kept on a branch."
            }
        }
    }
}

/// How the workweave index at the old path was written, which decides nothing
/// about the repair.
#[derive(Debug, Serialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyWorkweaveIndexKind {
    /// No `receipts` key: written before that key was kept.
    BeforeReceipts,
    /// Carries `receipts`, empty or not: the shape of the release that kept
    /// the index in the project checkout.
    OldPath,
}

impl LegacyWorkweaveIndexKind {
    pub const ALL: [Self; 2] = [Self::BeforeReceipts, Self::OldPath];

    pub fn severity(self) -> crate::integration::Severity {
        match self {
            Self::BeforeReceipts | Self::OldPath => crate::integration::Severity::Warning,
        }
    }

    pub fn fix_disposition(self) -> FixDisposition {
        match self {
            Self::BeforeReceipts | Self::OldPath => FixDisposition::Auto,
        }
    }

    pub(crate) fn of(shape: crate::workweave_index::LegacyShape) -> Self {
        match shape {
            crate::workweave_index::LegacyShape::BeforeReceipts => Self::BeforeReceipts,
            crate::workweave_index::LegacyShape::OldPath => Self::OldPath,
        }
    }
}
/// How a member's checkout stands against where its workspace expects it.
#[derive(Debug, Serialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AlignmentKind {
    /// On a branch other than the expected one, and nothing it holds would be
    /// left on no other ref if it moved off.
    OnBranch,
    /// On a branch other than the expected one, holding commits no other
    /// branch, remote-tracking branch or backup reaches.
    OnBranchWithStrayCommits,
    /// Detached where it is not expected to be, holding nothing a branch
    /// lacks.
    Detached,
    /// Detached, holding commits no branch or remote-tracking branch reaches.
    DetachedWithUnpushedCommits,
    /// The lock has no entry for the member and its expected branch is found
    /// nowhere.
    Unaligned,
    /// The project repo is on no branch, and its workspace was not made
    /// detached.
    ProjectDetached,
}

impl AlignmentKind {
    pub const ALL: [Self; 6] = [
        Self::OnBranch,
        Self::OnBranchWithStrayCommits,
        Self::Detached,
        Self::DetachedWithUnpushedCommits,
        Self::Unaligned,
        Self::ProjectDetached,
    ];

    pub fn severity(self) -> crate::integration::Severity {
        crate::integration::Severity::Warning
    }

    pub fn fix_disposition(self) -> FixDisposition {
        FixDisposition::ReportOnly
    }

    pub fn state(self) -> crate::diagnosis::State {
        use crate::diagnosis::State;
        match self {
            Self::OnBranch => State::AlignmentOnBranch,
            Self::OnBranchWithStrayCommits => State::AlignmentOnBranchWithStrayCommits,
            Self::Detached => State::AlignmentDetachedElsewhere,
            Self::DetachedWithUnpushedCommits => State::AlignmentDetachedUnpushed,
            Self::Unaligned => State::AlignmentUnaligned,
            Self::ProjectDetached => State::AlignmentProjectDetached,
        }
    }

    pub fn entry(self) -> &'static str {
        match self {
            Self::OnBranch => {
                "The checkout is on a branch the workspace does not expect, and moving it to \
                 the expected state leaves every commit it holds on some other ref. \
                 **What to do:** `rwv update`, which moves it."
            }
            Self::OnBranchWithStrayCommits => {
                "The checkout is on a branch the workspace does not expect, and it holds \
                 commits that no other branch, remote-tracking branch or backup reaches, \
                 listed. `rwv update` leaves it. **What to do:** copy the commits onto the \
                 expected branch, or keep them on a branch of their own and run `rwv update`; \
                 the message names the command."
            }
            Self::Detached => {
                "The checkout is detached where the workspace expects it on a branch, or at \
                 another commit than its pin, and no commit reaches it that a branch lacks. \
                 **What to do:** `rwv update`, which returns it."
            }
            Self::DetachedWithUnpushedCommits => {
                "The checkout is detached and holds commits no branch or remote-tracking \
                 branch reaches, listed. `rwv update` leaves it. **What to do:** keep the \
                 commits on a branch, then run `rwv update`; the message names the command."
            }
            Self::Unaligned => {
                "The lock has no entry for the member and its expected branch exists \
                 nowhere, so it stays where it is. **What to do:** pin it at a commit you \
                 choose, or start its branch at one."
            }
            Self::ProjectDetached => {
                "The project repo's HEAD is on no branch, and the workspace was not made \
                 detached, so no record says how its members should sit. The commits it \
                 holds that no branch reaches are listed. **What to do:** keep them on a \
                 branch if there are any, then switch the project repo back to a branch."
            }
        }
    }
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

/// Which way a member's tracked files disagree with its role.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionsKind {
    /// A `reference` member's tracked file carries a write bit.
    WritableReference,
    /// A tracked file of a member of any other role is not writable by its
    /// owner.
    ReadOnlyMember,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DriftKind {
    /// Nothing at the slot, and the canonical clone holds the pinned revision.
    Missing,
    /// Nothing at the slot, and the canonical clone does not hold the pinned
    /// revision, or is not on disk.
    MissingUnfetched,
    /// A symlink at the slot, dangling or not, and the canonical clone holds
    /// the pinned revision.
    Symlink,
    /// A symlink at the slot, and the canonical clone does not hold the pinned
    /// revision, or is not on disk.
    SymlinkUnfetched,
    /// A checkout exists for a path `rwv.lock` names and the manifest does
    /// not: a member removed by hand from `rwv.toml`.
    Extra,
    /// A checkout at the slot whose link to the canonical clone names a git
    /// directory that is not there, so git cannot open it; the canonical
    /// clone holds what relinking it needs — its registration, the
    /// workweave's branch, or the pinned revision.
    DanglingLink,
    /// A checkout at the slot whose link dangles, and the canonical clone is
    /// not on disk, or holds neither the workweave's branch nor the pinned
    /// revision.
    DanglingLinkUnfetched,
}

/// Which part of a project repo's lock merge driver setup is missing or left
/// as an earlier release wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MissingLockMergeDriverKind {
    /// The checked-out commit's `.gitattributes` assigns `rwv.lock` no driver.
    AttributeAbsent,
    /// It assigns `merge=rwv-ours` or `merge=ours`, written by an earlier
    /// release.
    AttributeLegacy,
    /// The clone's config does not define the driver's command.
    ConfigAbsent,
    /// The clone's config defines a driver that keeps one side.
    ConfigLegacy,
}

impl MissingLockMergeDriverKind {
    pub const ALL: [Self; 4] = [
        Self::AttributeAbsent,
        Self::AttributeLegacy,
        Self::ConfigAbsent,
        Self::ConfigLegacy,
    ];

    pub fn severity(self) -> crate::integration::Severity {
        crate::integration::Severity::Warning
    }

    pub fn fix_disposition(self) -> FixDisposition {
        FixDisposition::Auto
    }

    pub(crate) fn state(self) -> crate::diagnosis::State {
        use crate::diagnosis::State;
        match self {
            Self::AttributeAbsent => State::MissingLockMergeDriverAttributeAbsent,
            Self::AttributeLegacy => State::MissingLockMergeDriverAttributeLegacy,
            Self::ConfigAbsent => State::MissingLockMergeDriverConfigAbsent,
            Self::ConfigLegacy => State::MissingLockMergeDriverConfigLegacy,
        }
    }

    /// Every part of `setup` that is not as this release sets it up.
    pub(crate) fn of(setup: crate::vcs::LockDriverSetup) -> Vec<Self> {
        use crate::vcs::LockDriverPart;
        let declaration = match setup.declaration {
            LockDriverPart::Current => None,
            LockDriverPart::Absent => Some(Self::AttributeAbsent),
            LockDriverPart::Superseded => Some(Self::AttributeLegacy),
        };
        let definition = match setup.definition {
            LockDriverPart::Current => None,
            LockDriverPart::Absent => Some(Self::ConfigAbsent),
            LockDriverPart::Superseded => Some(Self::ConfigLegacy),
        };
        declaration.into_iter().chain(definition).collect()
    }

    pub fn entry(self) -> &'static str {
        match self {
            Self::AttributeAbsent => {
                "The checked-out branch's committed `.gitattributes` has no `rwv.lock \
                 merge=rwv-lock` line, so a merge rwv does not make merges the lock as text.\n\n\
                 **What to do:** `rwv doctor --fix`, run in the project's checkout. It adds the \
                 line to the working file and to the checked-out commit's file and commits only \
                 that line; any other edit to `.gitattributes` stays uncommitted, and anything \
                 else marked for the next commit stays marked. It declines, changing nothing, while a merge, rebase or \
                 cherry-pick is in progress in the project repo. Another branch carries its own \
                 file and is migrated where it is checked out."
            }
            Self::AttributeLegacy => {
                "The checked-out branch's committed `.gitattributes` assigns `rwv.lock` \
                 `merge=rwv-ours` or `merge=ours`, the driver an earlier release wrote, which \
                 kept one side of every lock merge. It is not honored.\n\n\
                 **What to do:** `rwv doctor --fix`, as above: it rewrites the line to \
                 `rwv.lock merge=rwv-lock`, committing only that."
            }
            Self::ConfigAbsent => {
                "The clone's config does not define `merge.rwv-lock.driver`. Config does not \
                 travel with a clone, so a clone rwv did not make has none; every workweave \
                 shares its clone's.\n\n\
                 **What to do:** `rwv doctor --fix`. It sets `merge.rwv-lock.driver` to \
                 `rwv lock --merge-driver %O %A %B`, which git runs by finding `rwv` on `PATH`."
            }
            Self::ConfigLegacy => {
                "The clone's config still defines `merge.rwv-ours.driver`, the driver an \
                 earlier release planted, or defines `merge.rwv-lock.driver` as a command other \
                 than `rwv lock --merge-driver %O %A %B`.\n\n\
                 **What to do:** `rwv doctor --fix`. It replaces the config: the earlier \
                 entries are removed and `merge.rwv-lock.driver` names rwv's command."
            }
        }
    }
}

/// Whether an uninitialized submodule's recorded commit is on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SubmoduleCommitKind {
    /// The canonical clone's copy of the submodule holds it. Auto-fixable:
    /// `--fix` checks the submodule out from there without the network.
    Fetched,
    /// Nothing local holds it. Report-only: `rwv update` in the workweave
    /// fetches it into the canonical clone and checks it out.
    Unfetched,
}

/// What stands at a stale worktree registration's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrationDirectoryKind {
    /// Nothing. Auto-fixable: `--fix` drops the registration, which points
    /// at nothing.
    DirectoryGone,
    /// A directory without the file that links it to the repo. Report-only:
    /// its files may be the operator's work, so `--fix` keeps the
    /// registration until the directory is moved away.
    DirectoryStands,
}

/// How a stale index should be treated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RecordDriftKind {
    /// The checkout's branch moved forward under it, and a path the move
    /// changed still has the moved-from tip's index entry. `--fix` gives it
    /// HEAD's; the moved-from tip, an ancestor of HEAD, keeps what it held.
    SafeToFix,
    /// An index entry differs from HEAD for a reason no move under the
    /// checkout shows: a staged change, a path both the move and the user
    /// changed, or an old tip's entry nothing records was left behind.
    /// `--fix` must not touch it.
    LiveRecorded,
}

/// How stale working-tree files should be treated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WorkingTreeDriftKind {
    /// The checkout's branch moved forward under it, and a file the move
    /// changed, with its index entry, still holds the moved-from tip's
    /// content. `--fix` gives it HEAD's; no work is lost.
    SafeToFix,
    /// A file differs from HEAD for a reason no move under the checkout
    /// shows: an edit, whatever commit already holds its content, a file both
    /// the move and the user changed, or an old tip's file nothing records was
    /// left behind. `--fix` must not touch it.
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
/// The registry is that somewhere else. It lives in the primary's project
/// clone, is written only by `rwv workweave create`, and records the absolute
/// path of every workweave it made. A tree the registry names is a workweave on the authority of a
/// file the tree does not contain and could not have forged by being copied.
///
/// Note what is deliberately NOT used as the discriminator: whether the tree
/// itself holds a workweave index. A workweave's project checkout shares its
/// clone's index with the primary, so holding one says nothing about identity.
/// Keying on it would classify real workweaves as unwitnessed and leave their stray
/// pointers unfixable.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename = "WeaveRootIdentityConflictKind")]
pub enum WeaveRootIdentityConflictKind<P> {
    /// The marker names THIS workspace's primary, and that primary's registry
    /// for the marker's project records THIS exact directory. External
    /// evidence settles it: the tree is a workweave, so `.rwv-active` is the
    /// redundant copy and deleting it destroys nothing the marker and the
    /// registry do not already say. Auto-fixable — `--fix` deletes the
    /// pointer and leaves the marker.
    RegisteredWorkweave {
        /// Project the marker names (and under whose registry it is recorded).
        project: ProjectName,
        /// Name the registry records this directory under.
        workweave_name: WorkweaveName,
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
        marker_path: P,
        /// Why the marker cannot witness its own claim.
        defect: crate::workspace::MarkerDefect<P>,
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

impl WeaveRootIdentityConflictKind<PathBuf> {
    pub fn into_wire(self) -> WeaveRootIdentityConflictKind<WirePath> {
        match self {
            Self::RegisteredWorkweave {
                project,
                workweave_name,
            } => WeaveRootIdentityConflictKind::RegisteredWorkweave {
                project,
                workweave_name,
            },
            Self::MarkerUnverifiable {
                marker_path,
                defect,
            } => WeaveRootIdentityConflictKind::MarkerUnverifiable {
                marker_path: WirePath::of(&marker_path),
                defect: defect.into_wire(),
            },
            Self::Unwitnessed { detail } => WeaveRootIdentityConflictKind::Unwitnessed { detail },
        }
    }
}

/// Discriminator for [`CheckViolation::WorkweaveTreeIntegrity`] findings.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename = "WorkweaveTreeIntegrityKind")]
pub enum WorkweaveTreeIntegrityKind<P> {
    /// The marker's `parent:` path no longer exists on disk. The workweave's
    /// parent was retired or deleted out-of-band (a crash mid-adopt, or a
    /// hand-deletion) while this child remained. The parent is provenance
    /// only. Auto-fixable: `rwv doctor --fix` re-points `parent` to primary
    /// (which always exists). Normal retire/delete adopts children before the
    /// parent is destroyed, so this only arises off the happy path.
    DanglingParent {
        /// The missing parent path recorded in the marker.
        parent_path: P,
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
    /// either (missing, or exists but is not a weave root) — e.g. an
    /// rsync'd workweave whose marker still points at the origin machine's
    /// absolute path. Report-only.
    ForeignPrimary {
        /// The primary path recorded in the marker (unresolved).
        marker_primary: P,
    },
    /// The marker's `primary:` path does not match this workspace, but
    /// resolves to a different, valid weave root — the normal shape
    /// when several weaves share one workweave container. Not a defect in
    /// this workweave, so excluded from the default text report: every
    /// sibling weave's doctor would otherwise repeat this about every other
    /// sibling. Still enumerated under `--json`.
    ForeignPrimaryOtherWorkspace {
        /// The other workspace's primary path (resolved).
        marker_primary: P,
    },
    /// A registered workweave entry whose recorded path is not a valid
    /// workweave (missing directory, missing marker, or marker validation
    /// fails). Auto-fixable: `rwv doctor --fix` prunes the stale entry.
    ///
    /// This surfaces both "workweave was deleted out-of-band with the
    /// registry left behind" and "index committed to VCS carries paths that
    /// are wrong on this machine" — the design's advisory-index doctrine
    /// depends on doctor catching both.
    StaleRegistryEntry {
        /// Project the stale entry belongs to.
        project: ProjectName,
        /// The recorded name of the workweave.
        workweave_name: WorkweaveName,
        /// The recorded absolute path (which no longer round-trips).
        recorded_path: P,
        /// Human-readable reason the entry failed validation.
        reason: String,
    },
    /// A marker-bearing directory in a workweave container whose
    /// `(project, name)` are NOT recorded in that project's workweave index.
    /// The workweave exists on disk but the
    /// primary-side registry does not know about it. Auto-fixable via
    /// `rwv doctor --fix` (adopts the entry into the registry) — the design
    /// requires operator-consented adoption, so read paths (`list`,
    /// `delete`) deliberately do NOT auto-adopt on the fly.
    UnregisteredWorkweave {
        /// Project this orphan workweave records in its marker.
        project: ProjectName,
        /// Workweave name parsed from the directory basename.
        workweave_name: WorkweaveName,
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
    /// container rather than directly in it: a multi-segment project name
    /// rendered with its `/` intact, as `chatly/web-app` + `wtest` at
    /// `<container>/chatly/web-app--wtest`, which leaves `chatly` behind as a
    /// directory the container scan reads as a stray. rwv renders that `/` as
    /// `+`, so the records spell `<container>/chatly+web-app--wtest`.
    ///
    /// The scan enumerates a container's immediate children, so nothing else
    /// in this check sees such a directory at all; this finding is emitted
    /// from the registry side, which records the path.
    ///
    /// Report-only, and the remedy is retire-and-recreate rather than a
    /// rename: the move crosses a directory boundary, which invalidates the
    /// worktree back-pointers of every checkout inside and the recorded path
    /// that found it. Workweaves are ephemeral by design; rwv does not
    /// migrate one in place.
    NestedWorkweaveDir {
        /// Project the entry belongs to.
        project: ProjectName,
        /// The recorded name of the workweave.
        workweave_name: WorkweaveName,
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

impl WorkweaveTreeIntegrityKind<PathBuf> {
    pub fn into_wire(self) -> WorkweaveTreeIntegrityKind<WirePath> {
        use WorkweaveTreeIntegrityKind as Wire;
        match self {
            Self::DanglingParent { parent_path } => Wire::DanglingParent {
                parent_path: WirePath::of(&parent_path),
            },
            Self::ParentChainAnomaly { detail } => Wire::ParentChainAnomaly { detail },
            Self::UnregisteredDir => Wire::UnregisteredDir,
            Self::ForeignPrimary { marker_primary } => Wire::ForeignPrimary {
                marker_primary: WirePath::of(&marker_primary),
            },
            Self::ForeignPrimaryOtherWorkspace { marker_primary } => {
                Wire::ForeignPrimaryOtherWorkspace {
                    marker_primary: WirePath::of(&marker_primary),
                }
            }
            Self::StaleRegistryEntry {
                project,
                workweave_name,
                recorded_path,
                reason,
            } => Wire::StaleRegistryEntry {
                project,
                workweave_name,
                recorded_path: WirePath::of(&recorded_path),
                reason,
            },
            Self::UnregisteredWorkweave {
                project,
                workweave_name,
            } => Wire::UnregisteredWorkweave {
                project,
                workweave_name,
            },
            Self::UnreadableMarker { detail } => Wire::UnreadableMarker { detail },
            Self::NestedWorkweaveDir {
                project,
                workweave_name,
                expected_dir_name,
            } => Wire::NestedWorkweaveDir {
                project,
                workweave_name,
                expected_dir_name,
            },
            Self::MisnamedDir {
                expected_dir_name,
                detail,
            } => Wire::MisnamedDir {
                expected_dir_name,
                detail,
            },
        }
    }
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
    /// The clone records its remote as a relative path. git reads one from
    /// the top of whichever checkout it runs in, and every workweave checkout
    /// of the member shares the canonical clone's record, so the path names a
    /// different remote from each. Warning severity; report-only.
    RelativeRemote {
        /// The path as the clone records it.
        path: String,
        /// The name the backend gives the remote, for the repair command.
        remote: String,
        /// The URL the manifest's `url` resolves to, when it resolves.
        manifest_remote: Option<String>,
    },
    /// The manifest's `url` is neither a URL nor registry shorthand, so it
    /// names no remote and no clone of the member can be made. Error
    /// severity; report-only.
    UnresolvableUrl {
        /// The `url` as `rwv.toml` spells it.
        manifest_url: String,
    },
    /// The manifest key disagrees with `placement(entry.url)` — the path the
    /// entry's own URL derives to under the canonical layout. Warning
    /// severity; report-only.
    ///
    /// Exempt by role rather than by survey: `reference` entries may point at
    /// a mirror on purpose (the same exemption `OriginUrlMismatch` already
    /// carries), and `fork` entries may key on the upstream's coordinates
    /// while the URL names the fork (`docs/reference/roles.md` §`fork`, "The
    /// path may name either"). Neither role reaches this variant at all —
    /// the scan skips them before deriving `expected_path`.
    PlacementDisagreement {
        /// The path `placement(entry.url)` derives — where the manifest key
        /// would need to be for the two to agree.
        expected_path: String,
        /// Where the move, re-key and `rwv lock` run, and what goes first.
        converge: PlacementConvergence,
    },
}

/// Where a [`ProvenanceKind::PlacementDisagreement`] is converged. Moving a
/// canonical clone strands every workweave worktree of it, so the move runs
/// in the primary weave, after each such workweave is landed and retired.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum PlacementConvergence {
    /// In this primary weave, once each workweave listed is retired.
    Here { retire_first: Vec<WorkweaveId> },
    /// From the primary weave; this is a workweave.
    FromPrimary,
}

/// Discriminator for [`CheckViolation::CloneTopology`] findings.
///
/// The four sub-kinds enumerate the ways the bottom tier of the stability
/// stack
/// ([clone-topology.md](../../docs/explanation/joints/clone-topology.md))
/// can break: a member repo's slot at `<weave>/<repo_path>` must be a
/// "canonical store" (a full clone), and every workweave checkout
/// `<workweave>/<repo_path>` must be a linked workspace whose VCS common
/// store resolves to that canonical store. Each variant names a distinct
/// way the on-disk shape diverges from that spec.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename = "CloneTopologyKind")]
pub enum CloneTopologyKind<P> {
    /// A full clone (its own canonical store) is hosted under `.workweaves/`
    /// instead of at the manifest's canonical slot. The inverted-primary
    /// case: the canonical store has migrated into one workweave and other
    /// workweaves' checkouts link into *it*, not into `<weave>/<repo_path>`.
    ///
    /// A member slot holding a symlink is not scanned: read through the link
    /// it is the canonical store itself. It is reported as `workweave-drift`.
    ///
    /// `--fix` replaces the clone with a worktree of the canonical clone on
    /// the workweave's ephemeral branch, at the commit the clone has checked
    /// out, unless the clone holds something the canonical clone would not
    /// give back: commits no canonical ref reaches, uncommitted, untracked or
    /// ignored files, or worktrees of its own.
    StandaloneInWorkweave {
        /// Absolute path of the standalone canonical store under
        /// `.workweaves/`.
        store_path: P,
    },
    /// The workspace at `<weave>/<repo_path>` is a full clone (its
    /// canonical store sits under itself), but one or more of this weave's
    /// workweave checkouts of the same repo resolve to a *different*
    /// canonical store. The weave-path clone publishes a separate object
    /// DAG nobody syncs to; push/pull becomes asymmetric and silent.
    DisconnectedWeaveClone {
        /// Absolute path of the canonical store at the weave slot (the
        /// "disconnected" one).
        weave_store_path: P,
        /// Absolute path of a representative store one of the workweave
        /// checkouts actually uses (the one this weave clone is
        /// disconnected from).
        other_store_path: P,
    },
    /// A linked worktree under `.workweaves/<workweave>/<repo_path>` whose
    /// canonical store is not the weave canonical at `<weave>/<repo_path>`.
    /// The shared-DAG invariant between the canonical and the workweave is
    /// broken: commits made here land in a different object store than the
    /// canonical, and merged-checks across the two answer "no" silently.
    WrongParentWorktree {
        /// Absolute path of the canonical store this workweave checkout
        /// should be linked into (`<weave>/<repo_path>/.git`).
        expected_store_path: P,
        /// Absolute path of the canonical store this workweave checkout
        /// is actually linked into.
        actual_store_path: P,
    },
    /// The weave path `<weave>/<repo_path>` itself is a linked worktree of
    /// some other clone — full inversion: there is no canonical store at
    /// the manifest slot, and the workspace there shares its DAG with
    /// whichever clone hosts the actual store.
    WeaveCloneIsWorktree {
        /// Absolute path of the canonical store this slot is linked into.
        actual_store_path: P,
    },
}

impl CloneTopologyKind<PathBuf> {
    pub fn into_wire(self) -> CloneTopologyKind<WirePath> {
        use CloneTopologyKind as Wire;
        match self {
            Self::StandaloneInWorkweave { store_path } => Wire::StandaloneInWorkweave {
                store_path: WirePath::of(&store_path),
            },
            Self::DisconnectedWeaveClone {
                weave_store_path,
                other_store_path,
            } => Wire::DisconnectedWeaveClone {
                weave_store_path: WirePath::of(&weave_store_path),
                other_store_path: WirePath::of(&other_store_path),
            },
            Self::WrongParentWorktree {
                expected_store_path,
                actual_store_path,
            } => Wire::WrongParentWorktree {
                expected_store_path: WirePath::of(&expected_store_path),
                actual_store_path: WirePath::of(&actual_store_path),
            },
            Self::WeaveCloneIsWorktree { actual_store_path } => Wire::WeaveCloneIsWorktree {
                actual_store_path: WirePath::of(&actual_store_path),
            },
        }
    }
}
/// How a detached HEAD stands against its workweave's existing branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DetachedAncestry {
    /// HEAD is at the branch's tip.
    Equal,
    /// The branch carries HEAD and commits HEAD lacks: switching to it
    /// strands nothing.
    HeadBehind,
    /// HEAD carries the branch and commits the branch lacks: only a
    /// fast-forward of the branch to HEAD strands nothing.
    HeadAhead,
    /// Each carries commits the other lacks, or their ancestry could not be
    /// read: switching strands HEAD's, and moving the branch strands its own.
    Diverged,
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
// The tag is the kebab-cased variant name, and `ViolationKind` lists the same
// names for the internal type.

/// One violation as it appears in `rwv doctor --json` output.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ViolationOutput {
    OrphanedClone {
        path: String,
        absolute_path: WirePath,
    },
    UnknownSegmentClone {
        path: String,
        absolute_path: WirePath,
    },
    DanglingReference {
        path: String,
        absolute_path: WirePath,
        project: ProjectName,
        /// The path the entry's URL places it at, when a clone of the same
        /// remote stands there and no entry of the project names it; `null`
        /// otherwise. Present, the repair is to re-key the entry to it, run
        /// `rwv lock` and pin it with `rwv member pin`; absent, it is `rwv update`.
        clone_at_placement: Option<String>,
    },
    MemberPermissions {
        path: String,
        absolute_path: WirePath,
        /// The workweave's identity, `<project>--<name>`; `None` for the primary weave.
        workweave: Option<WorkweaveId>,
        #[serde(rename = "sub_kind")]
        sub_kind: PermissionsKind,
        /// How many tracked regular files disagree with the member's role.
        files: usize,
    },
    RoleDisagreement {
        path: String,
        absolute_path: WirePath,
        /// Every project listing the clone, with the role it declares.
        roles: Vec<ProjectRoleOutput>,
    },
    HistoryDisagreement {
        path: String,
        absolute_path: WirePath,
        /// Every live weave branch, of every project listing the member, that
        /// records a `history` for it, with that value.
        histories: Vec<BranchHistoryOutput>,
    },
    Pin {
        path: String,
        absolute_path: WirePath,
        project: ProjectName,
        #[serde(rename = "sub_kind")]
        sub_kind: PinKind,
        /// The commit the lock pins; absent for `none` and `unreachable`.
        #[serde(skip_serializing_if = "Option::is_none")]
        locked: Option<String>,
        /// The commit the checkout is at; absent for `none` and `unreachable`.
        #[serde(skip_serializing_if = "Option::is_none")]
        actual: Option<String>,
        /// The commits only the checkout holds; empty unless `unpinned`.
        listed: Vec<String>,
    },
    WorkweaveDrift {
        path: String,
        absolute_path: WirePath,
        /// The workweave's identity, `<project>--<name>`; `None` for the canonical clone in the
        /// primary weave.
        workweave: Option<WorkweaveId>,
        #[serde(rename = "sub_kind")]
        sub_kind: DriftKind,
    },
    RecordDrift {
        path: String,
        absolute_path: WirePath,
        /// The workweave's identity, `<project>--<name>`; `None` for the primary weave.
        workweave: Option<WorkweaveId>,
        #[serde(rename = "sub_kind")]
        sub_kind: RecordDriftKind,
    },
    WorkingTreeDrift {
        path: String,
        absolute_path: WirePath,
        /// The workweave's identity, `<project>--<name>`; `None` for the primary weave.
        workweave: Option<WorkweaveId>,
        #[serde(rename = "sub_kind")]
        sub_kind: WorkingTreeDriftKind,
    },
    MissingLockMergeDriver {
        project: ProjectName,
        #[serde(rename = "sub_kind")]
        sub_kind: MissingLockMergeDriverKind,
    },
    LockMergeDriverUnreadable {
        project: ProjectName,
        error: String,
    },
    HeadUnreadable {
        path: String,
        absolute_path: WirePath,
        error: String,
    },
    ProjectsDirUnreadable {
        path: WirePath,
        error: String,
    },
    ProjectlessDir {
        absolute_path: WirePath,
    },
    UnnameableProject {
        absolute_path: WirePath,
        derived: String,
        error: String,
    },
    LockPinnedByName {
        path: String,
        absolute_path: WirePath,
        project: ProjectName,
        /// The tag or branch name the lock entry holds.
        version: String,
    },
    LegacyManifestFormat {
        project: ProjectName,
        legacy_path: WirePath,
    },
    DanglingActiveProject {
        project: ProjectName,
        missing_dir: WirePath,
    },
    UnreadableActiveProject {
        /// Absolute path of the `.rwv-active` file.
        path: WirePath,
        /// What is wrong with what it holds.
        detail: String,
    },
    WeaveRootIdentityConflict {
        /// Absolute path of the weave root carrying both identity files.
        root: WirePath,
        /// The project named by `.rwv-active`; absent when that file is
        /// empty or unreadable.
        pointer_project: Option<String>,
        #[serde(rename = "sub_kind")]
        sub_kind: WeaveRootIdentityConflictKind<WirePath>,
    },
    LegacyWorkweaveMarker {
        marker_path: WirePath,
        primary: WirePath,
    },
    LegacyWorkweaveIndex {
        project: ProjectName,
        index_path: WirePath,
        #[serde(rename = "sub_kind")]
        sub_kind: LegacyWorkweaveIndexKind,
    },
    UnreadableOwnedState {
        project: ProjectName,
        state_path: WirePath,
        error: String,
        active: bool,
    },
    UnreadableWorkweaveIndex {
        project: ProjectName,
        index_path: WirePath,
        error: String,
    },
    UnparseableProject {
        project: ProjectName,
        manifest_path: WirePath,
        /// Free-form display string of the parse error. Named `message`
        /// (not `error`) to signal this is display text, not a typed discriminant.
        message: String,
        #[serde(rename = "sub_kind")]
        sub_kind: UnparseableProjectKind,
    },
    WorkweaveTreeIntegrity {
        /// Absolute path to the workweave directory (or its marker file for
        /// file-level findings).
        workweave_dir: WirePath,
        /// Discriminator for the specific anomaly detected.
        #[serde(rename = "sub_kind")]
        sub_kind: WorkweaveTreeIntegrityKind<WirePath>,
    },
    Provenance {
        /// Manifest-relative path to the affected repo.
        path: String,
        /// Absolute path to the affected repo on disk.
        absolute_path: WirePath,
        /// Project the affected repo belongs to.
        project: ProjectName,
        /// Discriminator for the specific provenance anomaly.
        #[serde(rename = "sub_kind")]
        sub_kind: ProvenanceKind,
    },
    CloneTopology {
        /// Manifest-relative repo path (e.g. `github/cwalv/tmuxcc-broker`).
        path: String,
        /// Absolute path of the offending workspace (canonical slot or
        /// workweave checkout, per sub-kind semantics).
        absolute_path: WirePath,
        /// Discriminator for the specific topology anomaly.
        #[serde(rename = "sub_kind")]
        sub_kind: CloneTopologyKind<WirePath>,
    },
    Alignment {
        /// Absolute path to the checkout.
        repo_path: WirePath,
        project: ProjectName,
        #[serde(rename = "sub_kind")]
        sub_kind: AlignmentKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        listed: Vec<String>,
    },
    StaleWorktreeRegistration {
        path: String,
        absolute_path: WirePath,
        /// The workweave's identity, `<project>--<name>`; `None` for the primary weave.
        workweave: Option<WorkweaveId>,
        /// Absolute path of the registered worktree.
        worktree_path: WirePath,
        #[serde(rename = "sub_kind")]
        sub_kind: RegistrationDirectoryKind,
    },
    StaleOpState {
        /// Absolute path to the workspace dir that holds the `.rwv-op` file.
        workspace_dir: WirePath,
        /// The verb that started the stalled op — the one `--continue`
        /// resumes it under.
        verb: crate::op_state::OpVerb,
        /// Raw `started_at` string from the op-state file (RFC3339 UTC).
        started_at: String,
    },
    UnreadableOpRecord {
        /// Absolute path to the workspace dir that holds the `.rwv-op` file.
        workspace_dir: WirePath,
        /// Why it cannot be read, naming the verb that ends the operation.
        error: String,
    },
    DeadOpLease {
        /// Absolute path to the workspace dir holding the dangling lease.
        workspace_dir: WirePath,
        /// Op id recorded in the lease.
        op_id: String,
        /// Owner workspace the lease pointed at.
        recorded_owner: WirePath,
        /// Discriminator for the specific dead-lease shape.
        #[serde(rename = "sub_kind")]
        sub_kind: DeadOpLeaseKind,
        /// RFC3339 UTC timestamp at which the lease was written.
        /// Observability-only — never a decision input.
        #[serde(skip_serializing_if = "Option::is_none")]
        created_at: Option<String>,
    },
    ClaimOwnerUnreachable {
        /// Absolute path to the directory holding the claim file.
        claim_dir: WirePath,
        /// Op id the claim names.
        op_id: String,
        /// Owner workspace the claim names.
        recorded_owner: WirePath,
    },
    ClaimUnreadable {
        /// Absolute path to the claim or takeover guard.
        file: WirePath,
        /// What it holds, as text, invalid UTF-8 replaced.
        content: String,
    },
    ClaimUnopenable {
        /// Absolute path to the claim or takeover guard.
        file: WirePath,
        /// Rendered read error.
        error: String,
    },
    OrphanedSavepoint {
        path: String,
        absolute_path: WirePath,
        /// The workweave's identity, `<project>--<name>`; `None` for the primary weave.
        workweave: Option<WorkweaveId>,
        /// Opaque op-id from the savepoint ref's trailing path component.
        op_id: String,
        /// Safe-vs-live classification.
        #[serde(rename = "sub_kind")]
        sub_kind: OrphanedSavepointKind,
    },
    /// A ref under the savepoint namespace that no op id names. rwv did not
    /// file it, so `--fix` never drops it.
    UnrecognizedSavepointRef {
        path: String,
        absolute_path: WirePath,
        /// The workweave's identity, `<project>--<name>`; `None` for the primary weave.
        workweave: Option<WorkweaveId>,
        /// The reference, spelled as the VCS spells it.
        ref_name: String,
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
        weave_config: WirePath,
        /// Member-level `.cargo/config.toml` that wins per cargo's
        /// closest-config-wins-per-key shadowing.
        member_config: WirePath,
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
        absolute_path: WirePath,
        /// The workweave's identity, `<project>--<name>`.
        workweave: WorkweaveId,
        /// Absolute path of the canonical clone directory that is absent.
        canonical_path: WirePath,
    },

    /// See [`CheckViolation::UninitializedSubmodule`].
    UninitializedSubmodule {
        /// Absolute path to the repo worktree that has uninitialized submodules.
        absolute_path: WirePath,
        /// Manifest-relative path to the repo.
        path: String,
        /// The workweave's identity, `<project>--<name>`.
        workweave: WorkweaveId,
        #[serde(rename = "sub_kind")]
        sub_kind: SubmoduleCommitKind,
        /// Submodule paths (relative to the repo root) that are empty on disk.
        empty_paths: Vec<String>,
    },

    /// See [`CheckViolation::PhantomMergeDriver`].
    PhantomMergeDriver {
        /// Manifest-relative path to the repo carrying the `.gitattributes`.
        path: String,
        /// Absolute path to that repo on disk.
        absolute_path: WirePath,
        /// The path pattern the offending line assigns the driver to.
        pattern: String,
        /// The `rwv-`-prefixed driver name that resolves to nothing.
        driver: String,
    },
}

/// One project's declared role for a member.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct ProjectRoleOutput {
    pub project: ProjectName,
    pub role: Role,
}

/// The `history` one project's live weave branch records for a member.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct BranchHistoryOutput {
    pub project: ProjectName,
    pub branch: String,
    pub history: crate::manifest::History,
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
    /// The name `rwv explain` takes for this finding: its `kind`, or
    /// `<kind>/<sub-kind>` for a finding that carries a `sub_kind`.
    pub fn token(&self) -> String {
        let wire = serde_json::to_value(self).expect("a violation serializes");
        let kind = wire["kind"]
            .as_str()
            .expect("every violation carries a kind");
        match wire.get("sub_kind") {
            None | Some(serde_json::Value::Null) => kind.to_owned(),
            Some(sub_kind) => format!("{kind}/{}", crate::wire_token::wire_token(sub_kind)),
        }
    }

    /// Convert an internal [`CheckViolation`] into its wire-format
    /// counterpart, resolving `path` against `workspace_dir` for
    /// non-workweave variants and against `workweave_dirs` for
    /// workweave-scoped variants.
    pub fn from_violation(
        violation: CheckViolation,
        workspace_dir: &Path,
        workweave_dirs: &std::collections::HashMap<WorkweaveId, std::path::PathBuf>,
    ) -> Self {
        fn abs(workspace_dir: &Path, repo: &RepoPath) -> WirePath {
            WirePath::of(&workspace_dir.join(repo.as_path()))
        }
        fn abs_in(
            workweave: &Option<WorkweaveId>,
            workspace_dir: &Path,
            workweave_dirs: &std::collections::HashMap<WorkweaveId, std::path::PathBuf>,
            repo: &RepoPath,
        ) -> WirePath {
            let root = workweave
                .as_ref()
                .and_then(|ww| workweave_dirs.get(ww))
                .map(std::path::PathBuf::as_path)
                .unwrap_or(workspace_dir);
            WirePath::of(&root.join(repo.as_path()))
        }

        match violation {
            CheckViolation::OrphanedClone { path } => Self::OrphanedClone {
                absolute_path: abs(workspace_dir, &path),
                path: path.to_string(),
            },
            CheckViolation::UnknownSegmentClone { path } => Self::UnknownSegmentClone {
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
            CheckViolation::DanglingReference {
                project,
                repo,
                clone_at_placement,
            } => Self::DanglingReference {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.clone(),
                clone_at_placement: clone_at_placement.map(|placed| placed.to_string()),
            },
            CheckViolation::MemberPermissions {
                workweave,
                repo,
                kind,
                files,
            } => Self::MemberPermissions {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.clone(),
                sub_kind: kind,
                files,
            },
            CheckViolation::HistoryDisagreement { repo, histories } => Self::HistoryDisagreement {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                histories: histories
                    .into_iter()
                    .map(|recorded| BranchHistoryOutput {
                        project: recorded.project,
                        branch: recorded.branch,
                        history: recorded.history,
                    })
                    .collect(),
            },
            CheckViolation::RoleDisagreement { repo, roles } => Self::RoleDisagreement {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                roles: roles
                    .into_iter()
                    .map(|(project, role)| ProjectRoleOutput {
                        project: project.clone(),
                        role,
                    })
                    .collect(),
            },
            CheckViolation::Pin {
                project,
                repo,
                sub_kind,
                locked,
                actual,
                listed,
            } => Self::Pin {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.clone(),
                sub_kind,
                locked: locked.map(|rev| rev.display_str().to_owned()),
                actual: actual.map(|rev| rev.display_str().to_owned()),
                listed,
            },
            CheckViolation::WorkweaveDrift {
                workweave,
                kind,
                repo,
            } => Self::WorkweaveDrift {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.clone(),
                sub_kind: kind,
            },
            CheckViolation::RecordDrift {
                workweave,
                repo,
                kind,
            } => Self::RecordDrift {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.clone(),
                sub_kind: kind,
            },
            CheckViolation::WorkingTreeDrift {
                workweave,
                repo,
                kind,
            } => Self::WorkingTreeDrift {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.clone(),
                sub_kind: kind,
            },
            CheckViolation::MissingLockMergeDriver { project, sub_kind } => {
                Self::MissingLockMergeDriver {
                    project: project.clone(),
                    sub_kind,
                }
            }
            CheckViolation::LockMergeDriverUnreadable { project, error } => {
                Self::LockMergeDriverUnreadable {
                    project: project.clone(),
                    error,
                }
            }
            CheckViolation::HeadUnreadable { repo, error } => Self::HeadUnreadable {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                error,
            },
            CheckViolation::ProjectsDirUnreadable { path, error } => Self::ProjectsDirUnreadable {
                path: WirePath::of(&path),
                error,
            },
            CheckViolation::ProjectlessDir { dir } => Self::ProjectlessDir {
                absolute_path: WirePath::of(&dir),
            },
            CheckViolation::UnnameableProject {
                dir,
                derived,
                error,
            } => Self::UnnameableProject {
                absolute_path: WirePath::of(&dir),
                derived,
                error,
            },
            CheckViolation::LockPinnedByName {
                project,
                repo,
                version,
            } => Self::LockPinnedByName {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.clone(),
                version: version.as_str().to_owned(),
            },
            CheckViolation::LegacyManifestFormat {
                project,
                legacy_path,
            } => Self::LegacyManifestFormat {
                project: project.clone(),
                legacy_path: WirePath::of(&legacy_path),
            },
            CheckViolation::DanglingActiveProject {
                project,
                missing_dir,
            } => Self::DanglingActiveProject {
                project: project.clone(),
                missing_dir: WirePath::of(&missing_dir),
            },
            CheckViolation::UnreadableActiveProject { path, detail } => {
                Self::UnreadableActiveProject {
                    path: WirePath::of(&path),
                    detail: detail.clone(),
                }
            }
            CheckViolation::WeaveRootIdentityConflict {
                root,
                pointer_project,
                sub_kind,
            } => Self::WeaveRootIdentityConflict {
                root: WirePath::of(&root),
                pointer_project: pointer_project.map(|p| p.to_string()),
                sub_kind: sub_kind.into_wire(),
            },
            CheckViolation::LegacyWorkweaveMarker {
                marker_path,
                primary,
            } => Self::LegacyWorkweaveMarker {
                marker_path: WirePath::of(&marker_path),
                primary: WirePath::of(&primary),
            },
            CheckViolation::LegacyWorkweaveIndex {
                project,
                index_path,
                sub_kind,
            } => Self::LegacyWorkweaveIndex {
                project: project.clone(),
                index_path: WirePath::of(&index_path),
                sub_kind,
            },
            CheckViolation::UnreadableOwnedState {
                project,
                state_path,
                error,
                active,
            } => Self::UnreadableOwnedState {
                project: project.clone(),
                state_path: WirePath::of(&state_path),
                error,
                active,
            },
            CheckViolation::UnreadableWorkweaveIndex {
                project,
                index_path,
                error,
            } => Self::UnreadableWorkweaveIndex {
                project: project.clone(),
                index_path: WirePath::of(&index_path),
                error,
            },
            CheckViolation::UnparseableProject {
                project,
                manifest_path,
                message,
                sub_kind,
            } => Self::UnparseableProject {
                project: project.clone(),
                manifest_path: WirePath::of(&manifest_path),
                message,
                sub_kind,
            },
            CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir,
                sub_kind,
            } => Self::WorkweaveTreeIntegrity {
                workweave_dir: WirePath::of(&workweave_dir),
                sub_kind: sub_kind.into_wire(),
            },
            CheckViolation::Provenance {
                project,
                repo,
                sub_kind,
            } => Self::Provenance {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.clone(),
                sub_kind,
            },
            CheckViolation::CloneTopology {
                workspace_path,
                repo,
                sub_kind,
            } => Self::CloneTopology {
                absolute_path: WirePath::of(&workspace_path),
                path: repo.to_string(),
                sub_kind: sub_kind.into_wire(),
            },
            CheckViolation::Alignment {
                repo_path,
                project,
                sub_kind,
                branch,
                listed,
            } => Self::Alignment {
                repo_path: WirePath::of(&repo_path),
                project,
                sub_kind,
                branch,
                listed,
            },
            CheckViolation::StaleWorktreeRegistration {
                workweave,
                repo,
                worktree_path,
                sub_kind,
            } => Self::StaleWorktreeRegistration {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.clone(),
                worktree_path: WirePath::of(&worktree_path),
                sub_kind,
            },
            CheckViolation::StaleOpState {
                workspace_dir: ws_dir,
                verb,
                started_at,
            } => Self::StaleOpState {
                workspace_dir: WirePath::of(&ws_dir),
                verb,
                started_at,
            },
            CheckViolation::UnreadableOpRecord {
                workspace_dir: ws_dir,
                error,
            } => Self::UnreadableOpRecord {
                workspace_dir: WirePath::of(&ws_dir),
                error,
            },
            CheckViolation::DeadOpLease {
                workspace_dir: ws_dir,
                op_id,
                recorded_owner,
                sub_kind,
                created_at,
            } => Self::DeadOpLease {
                workspace_dir: WirePath::of(&ws_dir),
                op_id,
                recorded_owner: WirePath::of(&recorded_owner),
                sub_kind,
                created_at,
            },
            CheckViolation::ClaimOwnerUnreachable {
                claim_dir,
                op_id,
                recorded_owner,
            } => Self::ClaimOwnerUnreachable {
                claim_dir: WirePath::of(&claim_dir),
                op_id,
                recorded_owner: WirePath::of(&recorded_owner),
            },
            CheckViolation::ClaimUnreadable { file, seen } => Self::ClaimUnreadable {
                file: WirePath::of(&file),
                content: unreadable_claim_text(&seen),
            },
            CheckViolation::ClaimUnopenable { file, error } => Self::ClaimUnopenable {
                file: WirePath::of(&file),
                error,
            },
            CheckViolation::OrphanedSavepoint {
                workweave,
                repo,
                key,
                sub_kind,
            } => Self::OrphanedSavepoint {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.clone(),
                op_id: key.to_string(),
                sub_kind,
            },
            CheckViolation::UnrecognizedSavepointRef {
                workweave,
                repo,
                ref_name,
                ..
            } => Self::UnrecognizedSavepointRef {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.clone(),
                ref_name,
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
                weave_config: WirePath::of(&weave_config),
                member_config: WirePath::of(&member_config),
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
                    absolute_path: WirePath::of(&ww_dir.join(repo.as_path())),
                    path: repo.to_string(),
                    workweave: workweave.clone(),
                    canonical_path: WirePath::of(&canonical_path),
                }
            }
            CheckViolation::UninitializedSubmodule {
                workweave,
                repo,
                sub_kind,
                empty_paths,
            } => {
                let ww_dir = workweave_dirs.get(&workweave);
                let absolute_path = WirePath::of(
                    &ww_dir
                        .unwrap_or(&workspace_dir.to_path_buf())
                        .join(repo.as_path()),
                );
                Self::UninitializedSubmodule {
                    absolute_path,
                    path: repo.to_string(),
                    workweave: workweave.clone(),
                    sub_kind,
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
/// that carries fields is a single-key object whose key is the tag.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum IssueKindOutput {
    ToolMissing,
    ManagedFileMissing,
    ManagedFileDrift,
    ManagedFileUserHeld,
    Surfacing,
    ConfigRejected,
    MalformedSettings,
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
    pub path: WirePath,
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
    fn from_kind(kind: &IssueKind) -> Self {
        match kind {
            IssueKind::ToolMissing => Self::ToolMissing,
            IssueKind::ManagedFileMissing => Self::ManagedFileMissing,
            IssueKind::ManagedFileDrift => Self::ManagedFileDrift,
            IssueKind::ManagedFileUserHeld => Self::ManagedFileUserHeld,
            IssueKind::Surfacing => Self::Surfacing,
            IssueKind::ConfigRejected => Self::ConfigRejected,
            IssueKind::MalformedSettings => Self::MalformedSettings,
            IssueKind::MemberIncompatibility(observation) => {
                Self::MemberIncompatibility(MemberIncompatibilityOutput {
                    path: WirePath::of(observation.path()),
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

impl IssueKind {
    /// The kebab-case tag this kind travels under: what [`IssueKindOutput`]
    /// serializes it as, and what `--kind` names.
    pub fn tag(&self) -> String {
        crate::wire_token::wire_token(&IssueKindOutput::from_kind(self))
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
    /// Whether `rwv doctor --fix` acts on this finding. `false` for every
    /// finding of a kind `--fix` never repairs, and for content you hold the
    /// pen on that auto-repair would destroy.
    pub safe_to_fix: bool,
}

impl IssueOutput {
    /// Convert an internal [`Issue`] into its wire-format counterpart.
    pub fn from_issue(issue: Issue) -> Self {
        let safe_to_fix = issue.fix_permitted();
        Self {
            kind: IssueKindOutput::from_kind(&issue.kind),
            integration: issue.source.name().to_string(),
            severity: match issue.severity {
                crate::integration::Severity::Warning => SeverityOutput::Warning,
                crate::integration::Severity::Error => SeverityOutput::Error,
            },
            message: issue.message.to_string(),
            safe_to_fix,
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
        let entries = match std::fs::read_dir(container.as_path()) {
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
                    marker_path: crate::workspace::WorkweaveMarker::path_in(&dir).reported(),
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
/// Every project records its own basedir in its workweave index; the
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
                project: marker.project().clone(),
                workweave_name: name.clone(),
                expected_dir_name: WorkweaveId {
                    project: marker.project().clone(),
                    name: name.clone(),
                }
                .flat()
                .to_string(),
            },
        });
    }
    found
}

fn workweave_containers_for_scan(ws_root: &Path) -> BTreeSet<CanonicalPath> {
    let mut containers = BTreeSet::new();
    containers.insert(CanonicalPath::of(&crate::workweave_index::default_basedir(
        ws_root,
    )));
    for project in crate::workspace::discover_projects(ws_root) {
        if let Ok(Some(idx)) = crate::workweave_index::read(ws_root, &project) {
            if let Some(basedir) = idx.basedir {
                containers.insert(basedir);
            }
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
    let marker = crate::workspace::WorkweaveMarker::read(marker_dir)
        .with_context(|| {
            format!(
                "failed to read {}/.rwv-workweave for --fix",
                marker_dir.display()
            )
        })?
        .ok_or_else(|| {
            refusal(
                RefusalKind::RepairTargetChanged,
                format!(
                    "{}/.rwv-workweave vanished before --fix could re-point it",
                    marker_dir.display()
                ),
            )
        })?;

    // Race guard: if the parent already exists, the dangling condition healed
    // (e.g. the parent was recreated) — leave the marker alone.
    if marker.parent().as_path().exists() {
        return Ok(false);
    }

    crate::workspace::reparent(primary, marker_dir, marker, primary).with_context(|| {
        format!(
            "failed to re-point {}/.rwv-workweave during --fix",
            marker_dir.display()
        )
    })?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Pre-schema member-entry keys
// ---------------------------------------------------------------------------

/// What `--fix` did with a project's pre-schema `rwv.toml`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PreSchemaFix {
    /// The working file was rewritten; `committed` is whether HEAD's file was
    /// too, in a commit holding nothing else.
    Rewritten { committed: bool },
    /// Nothing was touched, for the reason given.
    Withheld(String),
}

/// Rename the pre-schema member-entry keys of the project's `rwv.toml`, in the
/// working file and in HEAD's, and commit only HEAD's rewrite.
///
/// HEAD's rewrite is staged as content rather than from the working file, so
/// the commit holds the rename and nothing else the working file carries, and
/// the working file is then rewritten with its other edits in place.
/// Withheld, with nothing touched, while an operation is in progress in the
/// project repo or anything but `rwv.toml` is staged: a commit would carry it.
pub(crate) fn fix_pre_schema_manifest(
    vcs: &dyn crate::vcs::Vcs,
    manifest_path: &Path,
) -> anyhow::Result<PreSchemaFix> {
    let file = Path::new(Manifest::FILE_NAME);
    let project_dir = manifest_path
        .parent()
        .with_context(|| format!("{} has no directory", manifest_path.display()))?;
    let working = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let Some(working_rewritten) = crate::manifest::rewrite_pre_schema_keys(&working)? else {
        return Ok(PreSchemaFix::Withheld(format!(
            "{} names no key an earlier release wrote",
            Manifest::FILE_NAME
        )));
    };
    let committed = vcs
        .head_revision(project_dir)
        .ok()
        .and_then(|head| vcs.read_file_at_revision(project_dir, &head, file).ok());
    let committed_rewritten = match committed {
        Some(text) => crate::manifest::rewrite_pre_schema_keys(&text)?,
        None => None,
    };
    if let Some(committed_rewritten) = &committed_rewritten {
        if let Some(op) = vcs.mid_operation(project_dir) {
            return Ok(PreSchemaFix::Withheld(format!(
                "a {} is in progress in the project repo; finish or abort it first",
                op.operation()
            )));
        }
        let staged = vcs.staged_paths(project_dir).with_context(|| {
            format!(
                "failed to read the paths marked for commit in {}",
                project_dir.display()
            )
        })?;
        if staged.iter().any(|path| path != Manifest::FILE_NAME) {
            return Ok(PreSchemaFix::Withheld(
                "the project repo has changes recorded for the next commit in other files, which the commit would \
                 carry; commit them or take them out of the VCS's record first"
                    .to_owned(),
            ));
        }
        vcs.stage_content(project_dir, file, committed_rewritten.as_bytes())
            .with_context(|| {
                format!(
                    "failed to mark {} for the commit in {}",
                    file.display(),
                    project_dir.display()
                )
            })?;
        vcs.commit(
            project_dir,
            "chore: rename rwv.toml member keys to branch and recurse-submodules (rwv doctor --fix)",
        )
        .with_context(|| format!("failed to commit in {}", project_dir.display()))?;
    }
    crate::durable_file::replace(manifest_path, working_rewritten.as_bytes())
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;
    Ok(PreSchemaFix::Rewritten {
        committed: committed_rewritten.is_some(),
    })
}

// ---------------------------------------------------------------------------
// Workweave-tree integrity scanning
// ---------------------------------------------------------------------------

/// Reconcile every project's workweave index against on-disk state.
///
/// Emits three registry-specific finding kinds:
///
/// * `stale-registry-entry` — a recorded name → path whose validation fails
///   (missing directory, missing / foreign / cross-project marker) or whose
///   marker disagrees with the entry. `rwv doctor --fix` restores the marker
///   where the worktree relation still holds, else prunes the entry.
/// * `unregistered-workweave` — a marker-bearing workweave present in a
///   container that is NOT recorded in that project's index. Adoptable via
///   `rwv doctor --fix`. Silent adoption in read paths is deliberately not
///   done — adoption is an operator-consented act.
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
    let presented = crate::workspace::observe_root(ws_root)
        .and_then(|observation| observation.presented_project().cloned());
    crate::workspace::discover_projects(ws_root)
        .into_iter()
        .filter_map(|project| {
            let project_dir = crate::workspace::project_dir(ws_root, &project);
            crate::owned_state::unreadable_ledger(&project_dir).map(|error| {
                let active = presented.as_ref() == Some(&project);
                CheckViolation::UnreadableOwnedState {
                    state_path: crate::owned_state::ledger_path(&project_dir).reported(),
                    project,
                    error,
                    active,
                }
            })
        })
        .collect()
}

fn scan_registry_reconciliation(ws_root: &Path) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    // Pass 1 — every recorded entry that fails validation is stale. Also
    // collect the set of validated (project, canonical-path) pairs so
    // pass 2 can identify unregistered orphans without double-reporting
    // ones the operator already recorded.
    let mut recorded_valid_paths: std::collections::HashSet<(String, CanonicalPath)> =
        std::collections::HashSet::new();
    let primary = CanonicalPath::of(ws_root);
    let mut unreadable_index_projects: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for project in crate::workspace::discover_projects(ws_root) {
        let legacy = crate::workweave_index::legacy_index_path(ws_root, &project);
        if legacy.exists() {
            unreadable_index_projects.insert(project.as_str().to_string());
            if let Err(e) = crate::workweave_index::legacy_shape(ws_root, &project) {
                violations.push(CheckViolation::UnreadableWorkweaveIndex {
                    project: project.clone(),
                    index_path: legacy.reported(),
                    error: format!("{e:#}"),
                });
            }
            continue;
        }
        let index = match crate::workweave_index::read(ws_root, &project) {
            Ok(Some(idx)) => Some(idx),
            Ok(None) => None,
            Err(e) => {
                unreadable_index_projects.insert(project.as_str().to_string());
                if let Some(index_path) = crate::workweave_index::index_path(ws_root, &project) {
                    violations.push(CheckViolation::UnreadableWorkweaveIndex {
                        project: project.clone(),
                        index_path: index_path.reported(),
                        error: format!("{e:#}"),
                    });
                }
                None
            }
        };
        for (name, entry) in index.iter().flat_map(|idx| idx.workweaves.iter()) {
            let path = &entry.path;
            let validation =
                crate::workweave::validate_registry_entry(&primary, &project, path.as_path());
            match validation {
                crate::workweave::RegistryEntryValidation::Valid => {
                    recorded_valid_paths.insert((project.as_str().to_string(), path.clone()));
                    if let Ok(Some(disagreement)) =
                        crate::workspace::disagreement_at(path.as_path())
                    {
                        violations.push(CheckViolation::WorkweaveTreeIntegrity {
                            workweave_dir: path.as_path().to_path_buf(),
                            sub_kind: WorkweaveTreeIntegrityKind::StaleRegistryEntry {
                                project: project.clone(),
                                workweave_name: name.clone(),
                                recorded_path: path.as_path().to_path_buf(),
                                reason: disagreement.what(),
                            },
                        });
                    }
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
                        workweave_dir: path.as_path().to_path_buf(),
                        sub_kind: WorkweaveTreeIntegrityKind::StaleRegistryEntry {
                            project: project.clone(),
                            workweave_name: name.clone(),
                            recorded_path: path.as_path().to_path_buf(),
                            reason,
                        },
                    });
                }
            }
        }
    }

    // Pass 2 — every marker-bearing workweave on disk (from any container)
    // that is not in the validated set is an orphan. Uses
    // `doctor_scan_container` for the shape-parse, iterating every unique
    // container so per-workweave overrides are covered. A project whose index
    // did not parse has no validated set to be absent from, so "orphan" is
    // not a fact about it — `unreadable-workweave-index` above is.
    let mut seen_orphans = std::collections::HashSet::new();
    for container in workweave_containers_for_scan(ws_root) {
        for (workweave, dir) in
            crate::workweave::doctor_scan_container(ws_root, container.as_path())
        {
            if unreadable_index_projects.contains(workweave.project.as_str()) {
                continue;
            }
            let canonical = CanonicalPath::of(&dir);
            let key = (workweave.project.as_str().to_string(), canonical.clone());
            if recorded_valid_paths.contains(&key) {
                continue;
            }
            if !seen_orphans.insert(canonical) {
                continue;
            }
            violations.push(CheckViolation::WorkweaveTreeIntegrity {
                workweave_dir: dir,
                sub_kind: WorkweaveTreeIntegrityKind::UnregisteredWorkweave {
                    project: workweave.project.clone(),
                    workweave_name: workweave.name.clone(),
                },
            });
        }
    }

    violations
}

/// Rebuild `project`'s unreadable workweave index from the markers of the
/// workweaves in the containers doctor scans: the names it lists, or `None`
/// where the index read by the time its claim was held.
pub fn rebuild_workweave_index(
    ws_root: &Path,
    project: &crate::manifest::ProjectName,
) -> anyhow::Result<Option<Vec<WorkweaveName>>> {
    let mut found = Vec::new();
    for container in workweave_containers_for_scan(ws_root) {
        for (workweave, dir) in
            crate::workweave::doctor_scan_container(ws_root, container.as_path())
        {
            if workweave.project != *project {
                continue;
            }
            let Ok(Some(marker)) = crate::workspace::WorkweaveMarker::read(&dir) else {
                continue;
            };
            found.push((
                workweave.name,
                CanonicalPath::of(&dir),
                marker.parent().clone(),
            ));
        }
    }
    let names = found.iter().map(|(name, ..)| name.clone()).collect();
    Ok(crate::workweave_index::rebuild(ws_root, project, &found)?.then_some(names))
}

/// Adopt an on-disk workweave into its project's workweave index, with the
/// parent its marker records.
///
/// Called by `rwv doctor --fix` for each `UnregisteredWorkweave` finding.
/// Idempotent: a race with a concurrent `workweave create` recording the
/// same entry is harmless (the writer replaces the entry with an
/// identical value).
pub fn fix_unregistered_workweave(
    ws_root: &Path,
    project: &crate::manifest::ProjectName,
    workweave_name: &WorkweaveName,
    workweave_dir: &Path,
) -> anyhow::Result<()> {
    let marker = crate::workspace::WorkweaveMarker::read(workweave_dir)?.ok_or_else(|| {
        refusal(
            RefusalKind::RepairTargetChanged,
            format!(
                "{}/.rwv-workweave vanished before --fix could adopt the workweave",
                workweave_dir.display()
            ),
        )
    })?;
    crate::workweave_index::record_workweave(
        ws_root,
        project,
        workweave_name,
        CanonicalPath::of(workweave_dir),
        marker.parent().clone(),
    )
}

/// What `rwv doctor --fix` did with a stale registry entry.
pub enum StaleEntryFix {
    /// The directory is still a worktree of the project clone: its marker
    /// was written again from the entry.
    MarkerRestored,
    /// Nothing the entry names is left to restore: the entry was removed.
    Pruned,
}

/// Repair a stale entry of a project's workweave index: restore the marker
/// from the entry where the worktree relation still holds, else prune the
/// entry.
///
/// Called by `rwv doctor --fix` for each `StaleRegistryEntry` finding.
/// Idempotent.
pub fn fix_stale_registry_entry(
    ws_root: &Path,
    project: &crate::manifest::ProjectName,
    workweave_name: &WorkweaveName,
) -> anyhow::Result<StaleEntryFix> {
    let primary = CanonicalPath::of(ws_root);
    let entry = crate::workweave_index::read(ws_root, project)?
        .and_then(|index| index.workweaves.get(workweave_name).cloned());
    if let Some(entry) = &entry {
        if crate::workspace::restore_marker_of_entry(&primary, project, entry)? {
            return Ok(StaleEntryFix::MarkerRestored);
        }
    }
    crate::workweave_index::forget_workweave(ws_root, project, workweave_name)?;
    Ok(StaleEntryFix::Pruned)
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
/// 5. **`stale-registry-entry`** — a workweave index entry whose recorded
///    path fails marker round-trip validation or whose marker disagrees with
///    it. Auto-fixable (`--fix` restores the marker where the worktree
///    relation holds, else prunes the entry).
/// 6. **`unregistered-workweave`** — a marker-bearing workweave present on
///    disk but absent from its project's index. Auto-fixable (`--fix`
///    adopts the entry). This is also the migration surface for
///    workspaces that predate the registry: run `rwv doctor --fix` to
///    self-heal the index from the on-disk workweaves.
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
        if let Ok(entries) = std::fs::read_dir(container.as_path()) {
            for e in entries.flatten() {
                if e.path().is_dir() {
                    roots.push(e.path());
                }
            }
        }
    }

    let primary = CanonicalPath::of(primary_root);
    let mut seen = BTreeSet::new();
    let mut violations = Vec::new();
    for root in roots {
        let canonical = CanonicalPath::of(&root);
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if let Some(v) = classify_weave_root_identity(&primary, &canonical) {
            violations.push(v);
        }
    }
    violations.sort_by_key(|v| match v {
        CheckViolation::WeaveRootIdentityConflict { root, .. } => root.clone(),
        _ => PathBuf::new(),
    });
    violations
}

/// What the primary root's `.rwv-active` holds, read by the observation
/// resolution reads. `None` when there is no pointer, or when the root carries
/// a marker and [`scan_weave_root_identity`] owns the report.
fn primary_pointer(primary: &Path) -> Option<crate::workspace::PointerContent> {
    use crate::workspace::{observe_root, ActivePointer, RootObservation};
    match observe_root(primary)? {
        RootObservation::Primary {
            pointer: ActivePointer::Present(content),
            ..
        } => Some(content),
        _ => None,
    }
}

/// Classify one candidate root: `None` when it is not carrying both files.
///
/// `observe_root` is the same reader `resolve` consumes, so this and
/// resolution cannot diverge on what a root *is*, and the two arms a pointer
/// beside a marker produces are the two this reports on. Neither turns on what
/// either file parses to: an empty pointer or a legacy marker is still a
/// present file and still two copies of one fact.
fn classify_weave_root_identity(
    primary: &CanonicalPath,
    root_id: &CanonicalPath,
) -> Option<CheckViolation> {
    use crate::workspace::{observe_root, ActivePointer, RootObservation};

    let root = root_id.as_path();
    let (pointer_project, marker) = match observe_root(root)? {
        RootObservation::MarkerUnverifiable {
            marker_path,
            defect,
            pointer: ActivePointer::Present(pointer),
            ..
        } => {
            return Some(CheckViolation::WeaveRootIdentityConflict {
                root: root.to_path_buf(),
                pointer_project: pointer.named_project().cloned(),
                sub_kind: WeaveRootIdentityConflictKind::MarkerUnverifiable {
                    marker_path: marker_path.reported(),
                    defect,
                },
            })
        }
        RootObservation::Disputed {
            marker, pointer, ..
        } => (pointer.named_project().cloned(), marker),
        _ => return None,
    };

    let unwitnessed = |detail: String| CheckViolation::WeaveRootIdentityConflict {
        root: root.to_path_buf(),
        pointer_project: pointer_project.clone(),
        sub_kind: WeaveRootIdentityConflictKind::Unwitnessed { detail },
    };

    if !marker.names_primary(primary) {
        return Some(unwitnessed(format!(
            "The marker names primary `{}`, which is not this workspace, so this \
             workspace's registry has no say over it.",
            marker.primary().as_path().display()
        )));
    }

    // The registry entry is the external witness: it lives at
    // the primary's project clone, is written only by `rwv workweave create`,
    // and names this directory by absolute path.
    let recorded = crate::workweave_index::read(primary.as_path(), marker.project())
        .ok()
        .flatten()
        .and_then(|idx| {
            idx.workweaves
                .into_iter()
                .find(|(_, entry)| entry.path == *root_id)
        });

    match recorded {
        Some((workweave_name, _)) => Some(CheckViolation::WeaveRootIdentityConflict {
            root: root.to_path_buf(),
            pointer_project,
            sub_kind: WeaveRootIdentityConflictKind::RegisteredWorkweave {
                project: marker.project().clone(),
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

pub fn scan_workweave_tree_integrity(ws_root: &Path) -> Vec<CheckViolation> {
    let ws_canonical = CanonicalPath::of(ws_root);

    let mut violations = Vec::new();

    // Emit registry-vs-disk reconciliation findings first: stale entries
    // (registered but not a valid workweave on disk), unregistered
    // workweaves (present on disk but not in the registry). These are the
    // findings the registry-based design added; the marker-integrity
    // checks that follow are unchanged from the pre-registry era except
    // that they now iterate over every recorded container.
    violations.extend(scan_registry_reconciliation(ws_root));
    violations.extend(scan_unreadable_owned_state(ws_root));

    // Enumerate every unique container to scan for marker-shape issues.
    let containers = workweave_containers_for_scan(ws_root);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for container in &containers {
        let entries = match std::fs::read_dir(container.as_path()) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_dir() {
                continue;
            }
            if seen.insert(CanonicalPath::of(&path)) {
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
        parent: CanonicalPath,
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
            let marker_primary = marker.primary().as_path().to_path_buf();
            let sub_kind = if crate::workspace::is_workspace_root(&marker_primary) {
                WorkweaveTreeIntegrityKind::ForeignPrimaryOtherWorkspace { marker_primary }
            } else {
                WorkweaveTreeIntegrityKind::ForeignPrimary { marker_primary }
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
                    let expected_dir = WorkweaveId {
                        project: marker.project().clone(),
                        name: name.clone(),
                    }
                    .flat();
                    if basename != expected_dir.as_str() {
                        violations.push(CheckViolation::WorkweaveTreeIntegrity {
                            workweave_dir: dir.clone(),
                            sub_kind: WorkweaveTreeIntegrityKind::MisnamedDir {
                                expected_dir_name: Some(expected_dir.to_string()),
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
            parent: marker.parent().clone(),
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
    let dir_lookup: std::collections::HashMap<
        CanonicalPath,
        (crate::manifest::ProjectName, CanonicalPath),
    > = marker_entries
        .iter()
        .filter_map(|e| {
            let canon = CanonicalPath::existing(&e.dir).ok()?;
            Some((canon, (e.project.clone(), e.parent.clone())))
        })
        .collect();

    for entry in &marker_entries {
        let dir_canon = match CanonicalPath::existing(&entry.dir) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // (a) parent == self
        let parent_canon = entry.parent.clone();
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
        let mut visited = std::collections::HashSet::new();
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
                            current_parent.as_path().display()
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
                                current_parent.as_path().display(),
                                parent_project.as_str(),
                                entry.project.as_str()
                            ),
                        },
                    });
                    break;
                }
                // Keep climbing.
                current_parent = next_parent.clone();
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
    if !is_enabled(&integration, ctx.config()) {
        return Ok(Vec::new());
    }

    let cfg: CargoWorkspaceConfig = ctx.config().settings()?;
    // Reuse the integration's enablement predicate for "does this workspace
    // have cargo work at all?" so the scan silently no-ops in workspaces
    // without any Rust members.
    let members = CargoWorkspace::scan_members(ctx, &cfg)?;
    if members.is_empty() {
        return Ok(Vec::new());
    }

    let mut violations = Vec::new();
    for (crate_name, occurrences) in
        CargoWorkspace::scan_version_skew(ctx.workspace_root(), &members)
    {
        violations.push(CheckViolation::CargoVersionSkew {
            crate_name,
            occurrences,
        });
    }
    for rec in
        CargoWorkspace::scan_patch_shadowing(ctx.output_dir(), ctx.workspace_root(), &members)
    {
        violations.push(CheckViolation::CargoPatchShadowing {
            weave_config: rec.weave_config,
            member_config: rec.member_config,
            registry: rec.registry,
            crate_name: rec.crate_name,
        });
    }
    Ok(violations)
}

/// Where the clone at `clone` converges a placement disagreement: from the
/// primary weave when doctor runs in a workweave, and otherwise here, after
/// every workweave holding a worktree of it.
fn placement_convergence(
    ctx: &crate::workspace::WorkspaceContext,
    clone: &Path,
    vcs_type: crate::manifest::VcsType,
) -> PlacementConvergence {
    if !matches!(ctx.checkout, crate::workspace::Checkout::Primary { .. }) {
        return PlacementConvergence::FromPrimary;
    }
    let worktrees = crate::vcs::vcs_for(vcs_type)
        .list_worktrees(clone)
        .unwrap_or_default();
    let retire_first = crate::workweave::list_workweave_dirs(ctx.primary_path())
        .into_iter()
        .filter(|(_, dir)| {
            let dir = CanonicalPath::of(dir);
            worktrees
                .iter()
                .any(|worktree| dir.contains(&CanonicalPath::of(worktree)))
        })
        .map(|(workweave, _)| workweave)
        .collect();
    PlacementConvergence::Here { retire_first }
}

/// The url the member's entry has on `D`, the branch a fresh clone of the
/// project checks out: the url it was added with. `None` where `D` cannot be
/// read or does not list the member.
fn url_on_default_branch(
    workspace_dir: &Path,
    project: &crate::manifest::ProjectName,
    repo_path: &crate::manifest::RepoPath,
) -> Option<crate::manifest::RepoUrl> {
    let vcs = crate::vcs::project_vcs();
    let dir = crate::workspace::project_dir(workspace_dir, project);
    if !vcs.is_repo(&dir) {
        return None;
    }
    let remote = vcs.project_url_remote(&dir).ok()??;
    let default = vcs.remote_default_branch(&dir, &remote).ok()??;
    let branch = crate::vcs::TrackingRef::try_from(&default).ok()?;
    let raw = crate::vcs::RawRefName::new(branch.local_counterpart().as_str());
    let tip = vcs.resolve_local_branch_tip(&dir, &raw).ok()?.or_else(|| {
        vcs.remote_copies(&dir, &raw)
            .ok()?
            .into_iter()
            .find(|copy| copy.remote == remote)
            .map(|copy| copy.tip)
    })?;
    let text = vcs
        .read_file_at_revision(&dir, &tip, Path::new(crate::manifest::Manifest::FILE_NAME))
        .ok()?;
    let manifest = crate::manifest::Manifest::from_other_commit(&text).ok()?;
    manifest.get_entry(repo_path).map(|entry| entry.url.clone())
}

/// Scan all repos in `projects` for provenance violations.
///
/// Checks performed:
///
/// 1. **`origin-url-mismatch`** — the clone's `origin` remote URL differs
///    from the URL recorded in the manifest. Warning severity; report-only.
///    Reference-role repos may intentionally diverge (see note in violation
///    message). Only repos that exist on disk are checked.
///
/// 2. **`placement-disagreement`** — the manifest key disagrees with
///    `placement(entry.url)`. Warning severity; report-only. A pure manifest
///    comparison, so unlike the check above it does not require the
///    repo to exist on disk. `reference` and `fork` entries are exempt by
///    role (see [`ProvenanceKind::PlacementDisagreement`]) and never reach
///    the comparison; an entry whose URL derives no path at all (`placement`
///    returns `None`) has nothing to compare against and is skipped too.
///
/// 3. **`unresolvable-url`** — the manifest's `url` resolves to no remote.
///    Error severity; report-only. A pure manifest check.
///
/// 4. **`relative-remote`** — the clone records its remote as a relative
///    path, which names no one remote. Warning severity; report-only. It
///    takes the place of the `origin-url-mismatch` comparison for that clone.
pub fn scan_provenance(
    ctx: &crate::workspace::WorkspaceContext,
    workspace_dir: &Path,
    projects: &[Project],
) -> Vec<CheckViolation> {
    use crate::manifest::Role;
    use crate::registry::{recorded_remotes, resolve_to_clone_info, RecordedRemote};
    use crate::vcs::vcs_for;

    let mut violations = Vec::new();

    for project in projects {
        // --- placement-disagreement ---
        for (repo_path, entry) in project.manifest.iter_entries() {
            if matches!(entry.role, Role::Reference | Role::Fork) {
                continue;
            }
            let on_default = url_on_default_branch(workspace_dir, &project.name, repo_path);
            let Some(expected) =
                crate::registry::placement(on_default.as_ref().unwrap_or(&entry.url))
            else {
                continue;
            };
            if &expected != repo_path {
                violations.push(CheckViolation::Provenance {
                    project: project.name.clone(),
                    repo: repo_path.clone(),
                    sub_kind: ProvenanceKind::PlacementDisagreement {
                        expected_path: expected.as_str().to_string(),
                        converge: placement_convergence(
                            ctx,
                            &workspace_dir.join(repo_path.as_path()),
                            entry.vcs_type,
                        ),
                    },
                });
            }
        }

        // --- unresolvable-url ---
        for (repo_path, entry) in project.manifest.iter_entries() {
            if resolve_to_clone_info(&entry.url).is_err() {
                violations.push(CheckViolation::Provenance {
                    project: project.name.clone(),
                    repo: repo_path.clone(),
                    sub_kind: ProvenanceKind::UnresolvableUrl {
                        manifest_url: entry.url.to_string(),
                    },
                });
            }
        }

        // --- origin-url-mismatch, relative-remote ---
        for (repo_path, entry) in project.manifest.iter_entries() {
            let vcs = vcs_for(entry.vcs_type);
            let repo_abs = workspace_dir.join(repo_path.as_path());
            if !repo_abs.is_dir() || crate::workweave::is_dangling_link(&repo_abs) {
                continue;
            }

            let recorded = match recorded_remotes(vcs.as_ref(), &repo_abs) {
                Ok(r) if !r.is_empty() => r,
                _ => continue, // no remote recorded, or none readable — not this check's concern
            };
            let declared = resolve_to_clone_info(&entry.url).ok().map(|info| info.url);

            let relative: Vec<ProvenanceKind> = recorded
                .iter()
                .filter_map(|(remote, recorded)| match recorded {
                    RecordedRemote::RelativePath(path) => Some(ProvenanceKind::RelativeRemote {
                        path: path.clone(),
                        remote: remote.to_string(),
                        manifest_remote: declared.as_ref().map(|url| url.to_string()),
                    }),
                    RecordedRemote::Url(_) => None,
                })
                .collect();
            let sub_kinds = if !relative.is_empty() {
                relative
            } else {
                let Some(declared) = declared else {
                    // A url that resolves to no remote has nothing to compare.
                    continue;
                };
                let urls: Vec<&crate::registry::CloneUrl> = recorded
                    .iter()
                    .filter_map(|(_, recorded)| match recorded {
                        RecordedRemote::Url(url) => Some(url),
                        RecordedRemote::RelativePath(_) => None,
                    })
                    .collect();
                if urls.iter().any(|actual| declared.names_same_remote(actual)) {
                    continue;
                }
                vec![ProvenanceKind::OriginUrlMismatch {
                    manifest_url: entry.url.to_string(),
                    actual_url: urls.first().map(|u| u.to_string()).unwrap_or_default(),
                    is_reference_role: entry.role == Role::Reference,
                }]
            };
            for sub_kind in sub_kinds {
                violations.push(CheckViolation::Provenance {
                    project: project.name.clone(),
                    repo: repo_path.clone(),
                    sub_kind,
                });
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
/// config. The derived-content driver is defined only per invocation, by the
/// `-c` flags a stated derived-content policy contributes to a single git
/// command, which leaves nothing on disk to detect: a config probe would
/// answer "no such driver" for a name rwv defines every time it replays. The
/// static answer holds in every repo at every moment, which is what a check
/// the operator is meant to act on needs.
fn rwv_defines_merge_driver(name: &str) -> bool {
    name == crate::git::RWV_MERGE_DRIVER_NAME || name == crate::git::DERIVED_CONTENT_DRIVER_NAME
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
/// Reads the working-tree `.gitattributes`: the operator's next commit is the
/// one worth catching, and a file read costs no subprocess. Report-only —
/// repairing a phantom means guessing which driver the operator meant, or
/// that they meant nothing at all.
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
        let project_repo = RepoPath::new(project_rel_path(&project.name)).ok();
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
/// tokens that follow, honoring the double-quoted form gitattributes(5)
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
/// resolved through [`crate::vcs::Vcs::store_of`] (intent:
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
/// The scanner is read-only. Only `standalone-in-workweave` has a `--fix`
/// arm, `repair_standalone_members`; the other three need an object-store
/// migration.
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
    let mut violations = Vec::new();
    if repo_paths.is_empty() {
        return violations;
    }

    // Collect every workweave under this weave once; we iterate per-repo
    // inside the loop.
    let workweaves = crate::workweave::list_workweave_dirs(ws_root);

    for repo in repo_paths {
        let canonical_slot = ws_root.join(repo.as_path());
        let canonical_store_raw = vcs.store_of(&canonical_slot);

        // Expected canonical store path: `<canonical_slot>/.git`. Compare via
        // canonicalize to absorb any trailing-slash / symlink differences.
        let expected_store = crate::git::store_path_in(&canonical_slot);
        let expected_store_canon = CanonicalPath::of(&expected_store);

        let canonical_store_canon = canonical_store_raw.as_deref().map(CanonicalPath::of);

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
                            .unwrap_or_else(|| canonical_store_canon_pb.as_path().to_path_buf()),
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

            // Reached through the link, a symlinked member reads as the
            // canonical store itself and would classify as
            // `StandaloneInWorkweave`. It is reported as `workweave-drift`
            // instead.
            if crate::workweave::is_symlinked_member(&ww_checkout) {
                continue;
            }

            let ww_store_raw = match vcs.store_of(&ww_checkout) {
                Some(p) => p,
                None => continue, // not a workspace there; skip silently
            };
            let ww_store_canon = CanonicalPath::of(&ww_store_raw);

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
            if crate::workweave::is_own_store(&ww_store_raw, &ww_checkout) {
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
                    if CanonicalPath::of(&rep) != expected_store_canon {
                        violations.push(CheckViolation::CloneTopology {
                            workspace_path: canonical_slot.clone(),
                            repo: repo.clone(),
                            sub_kind: CloneTopologyKind::DisconnectedWeaveClone {
                                weave_store_path: canonical_store_raw.clone().unwrap_or_else(
                                    || expected_store_canon.as_path().to_path_buf(),
                                ),
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

/// Scan every workspace's member checkouts against where the workspace's
/// checked-out branch says they should be.
///
/// One pass over the primary and each workweave of each project: the engine
/// that `rwv update` aligns with judges each checkout, and a checkout it
/// would not leave alone is a finding. A workspace an operation holds is
/// skipped, as is a checkout stopped in the middle of an operation of its
/// own: git refuses to switch branches mid-rebase, and moving HEAD
/// mid-bisect abandons the bisect.
pub fn scan_alignment(
    ws_root: &Path,
    projects: &[Project],
    unjudged: &mut Vec<String>,
) -> Vec<CheckViolation> {
    let mut out = Vec::new();
    let accounting = match crate::alignment::Accounting::scan(ws_root) {
        Ok(accounting) => accounting,
        Err(e) => {
            unjudged.push(format!(
                "what the weave's live branches declare could not be read: {e:#}"
            ));
            return out;
        }
    };
    for project in projects {
        let mut workspaces: Vec<PathBuf> = vec![ws_root.to_path_buf()];
        workspaces.extend(
            crate::workweave::list_workweave_dirs(ws_root)
                .into_iter()
                .filter(|(id, _)| id.project == project.name)
                .map(|(_, dir)| dir),
        );
        for workspace in workspaces {
            scan_workspace_alignment(
                ws_root,
                &workspace,
                project,
                &accounting,
                &mut out,
                unjudged,
            );
        }
    }
    out
}

fn scan_workspace_alignment(
    ws_root: &Path,
    workspace: &Path,
    project: &Project,
    accounting: &crate::alignment::Accounting,
    out: &mut Vec<CheckViolation>,
    unjudged: &mut Vec<String>,
) {
    use crate::alignment::{Aligner, Member};
    use crate::diagnosis::State;

    let held = !matches!(
        (
            crate::op_state::read_owner(workspace),
            crate::op_state::read_lease(workspace),
        ),
        (Ok(None), Ok(None))
    );
    if held {
        return;
    }
    let project_dir = project_dir(workspace, &project.name);
    let Ok(manifest) = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME)) else {
        return;
    };
    let lock = LockFile::from_state(&LockFile::path_in(&project_dir)).ok();
    let aligner = match Aligner::open(workspace, &project.name, accounting.clone()) {
        Ok(aligner) => aligner,
        Err(e) => {
            unjudged.push(format!(
                "{}: the alignment of its workspace could not be read: {e:#}",
                project_dir.display()
            ));
            return;
        }
    };
    if aligner.standing == crate::alignment::ProjectStanding::Detached {
        let vcs = crate::vcs::project_vcs();
        if matches!(vcs.set_back_from(&project_dir), Ok(Some(_))) {
            return;
        }
        let listed = vcs
            .head_revision(&project_dir)
            .ok()
            .and_then(|head| vcs.stray_commits(&project_dir, &head, None, None).ok())
            .unwrap_or_default()
            .iter()
            .map(|c| {
                format!(
                    "{} {}",
                    crate::vcs::short_revision(c.id.as_str()),
                    c.subject
                )
            })
            .collect();
        out.push(CheckViolation::Alignment {
            repo_path: project_dir,
            project: project.name.clone(),
            sub_kind: AlignmentKind::ProjectDetached,
            branch: None,
            listed,
        });
        return;
    }
    for (repo_path, entry) in manifest.iter_entries() {
        let dir = workspace.join(repo_path.as_path());
        if !dir.is_dir() || crate::workweave::is_symlinked_member(&dir) {
            continue;
        }
        let member_vcs = crate::vcs::vcs_for(entry.vcs_type);
        if member_vcs.mid_operation(&dir).is_some() {
            continue;
        }
        let standalone = workspace != ws_root
            && member_vcs
                .store_of(&dir)
                .is_some_and(|store| crate::workweave::is_own_store(&store, &dir));
        if standalone {
            continue;
        }
        let member = Member {
            path: repo_path,
            entry,
            dir: &dir,
            vcs: member_vcs.as_ref(),
        };
        let lines = match aligner.status_lines(&member, lock.as_ref(), repo_path) {
            Ok(lines) => lines,
            Err(e) => {
                unjudged.push(format!(
                    "{}: its alignment could not be read: {e:#}",
                    dir.display()
                ));
                continue;
            }
        };
        for line in lines {
            let sub_kind = match line.state() {
                State::AlignmentOnBranch => AlignmentKind::OnBranch,
                State::AlignmentOnBranchWithStrayCommits => AlignmentKind::OnBranchWithStrayCommits,
                State::AlignmentDetachedElsewhere => AlignmentKind::Detached,
                State::AlignmentDetachedUnpushed => AlignmentKind::DetachedWithUnpushedCommits,
                State::AlignmentUnaligned => AlignmentKind::Unaligned,
                _ => continue,
            };
            out.push(CheckViolation::Alignment {
                repo_path: dir.clone(),
                project: project.name.clone(),
                sub_kind,
                branch: line.subject().map(ToOwned::to_owned),
                listed: line.listed().to_vec(),
            });
        }
    }
}

/// Scan the member checkouts of every workweave [`workweaves_in_scope`]
/// names for uninitialized submodules.
///
/// A member whose manifest entry says `recurse-submodules = false` is left out, and
/// so is a submodule its repository's own configuration declines: their
/// empty directories are what the operator asked for. Each checkout yields
/// up to two findings, one per [`SubmoduleCommitKind`].
///
/// **Cost**: one `Path::exists` call per repo per workweave when `.gitmodules`
/// is absent (the common case). No network I/O.
fn scan_uninitialized_submodules_in_workweaves(
    ctx: &crate::workspace::WorkspaceContext,
    project_scope: Option<&ProjectName>,
) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    for (workweave, workweave_dir, loaded) in workweaves_in_scope(ctx, project_scope) {
        for (repo_path, entry) in loaded.manifest.iter_entries() {
            if entry.recurse_submodules == crate::manifest::SubmoduleRecursion::Skip {
                continue;
            }
            let worktree = workweave_dir.join(repo_path.as_path());
            if crate::workweave::is_symlinked_member(&worktree) || !worktree.is_dir() {
                continue;
            }
            let empty_paths = crate::workweave::scan_uninitialized_submodules(&worktree);
            if empty_paths.is_empty() {
                continue;
            }
            let vcs = crate::vcs::vcs_for(entry.vcs_type);
            let (fetched, unfetched) =
                partition_uninitialized(vcs.as_ref(), &worktree, empty_paths);
            for (sub_kind, empty_paths) in [
                (SubmoduleCommitKind::Fetched, fetched),
                (SubmoduleCommitKind::Unfetched, unfetched),
            ] {
                if !empty_paths.is_empty() {
                    violations.push(CheckViolation::UninitializedSubmodule {
                        workweave: workweave.clone(),
                        repo: repo_path.clone(),
                        sub_kind,
                        empty_paths,
                    });
                }
            }
        }
    }

    violations
}

/// The empty submodule directories of `worktree` its configuration does not
/// decline, split by whether the canonical clone's copy holds the recorded
/// commit.
fn partition_uninitialized(
    vcs: &dyn crate::vcs::Vcs,
    worktree: &Path,
    empty_paths: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let recorded = vcs
        .population()
        .and_then(|population| population.recorded_submodules(worktree).ok())
        .unwrap_or_default();
    let store = vcs.store_of(worktree);
    let mut fetched = Vec::new();
    let mut unfetched = Vec::new();
    for path in empty_paths {
        let submodule = recorded.iter().find(|s| s.path == path);
        if submodule.is_some_and(|s| s.checkout == crate::vcs::SubmoduleCheckout::Declined) {
            continue;
        }
        let held = match (vcs.population(), &store, submodule) {
            (Some(population), Some(store), Some(submodule)) => {
                population.module_data_holds(store, submodule)
            }
            _ => false,
        };
        if held {
            fetched.push(path);
        } else {
            unfetched.push(path);
        }
    }
    (fetched, unfetched)
}

/// `--fix` for `uninitialized-submodule` / `fetched` in the workweaves
/// [`workweaves_in_scope`] names: check out each empty submodule of a member
/// that does not opt out, from the canonical clone's copies. What cannot be
/// checked out stays, and the scan that follows reports it.
fn repair_uninitialized_submodules(
    ctx: &crate::workspace::WorkspaceContext,
    project_scope: Option<&ProjectName>,
) {
    for (workweave, workweave_dir, loaded) in workweaves_in_scope(ctx, project_scope) {
        for (repo, entry) in loaded.manifest.iter_entries() {
            let member = workweave_dir.join(repo.as_path());
            if entry.recurse_submodules == crate::manifest::SubmoduleRecursion::Skip
                || crate::workweave::is_symlinked_member(&member)
                || !member.is_dir()
                || crate::workweave::scan_uninitialized_submodules(&member).is_empty()
            {
                continue;
            }
            let vcs = crate::vcs::vcs_for(entry.vcs_type);
            let populated = crate::share::populate_submodules(
                vcs.as_ref(),
                &member,
                entry.role,
                &crate::share::Filesystem,
            );
            for (path, placement) in &populated.populated {
                outln!(
                    "[fixed] core: checked out submodule {repo}/{path} in workweave {workweave} \
                     ({placement})"
                );
            }
            for (path, why) in &populated.left {
                eprintln!(
                    "rwv doctor: {repo}/{path} in workweave {workweave}: submodule left \
                     uninitialized: {why}"
                );
            }
        }
    }
}

/// The workweaves doctor answers for, each with its own project: the one it
/// runs in, or, from the primary, each registered workweave of a project in
/// scope. A workweave no registry entry names has no name to act under; the
/// tree-integrity scan reports it.
fn workweaves_in_scope(
    ctx: &crate::workspace::WorkspaceContext,
    project_scope: Option<&ProjectName>,
) -> Vec<(WorkweaveId, PathBuf, Project)> {
    use crate::workspace::Checkout;

    let workweave_dirs: Vec<PathBuf> = match &ctx.checkout {
        Checkout::Workweave { .. } => vec![ctx.active_path().to_path_buf()],
        Checkout::Primary { .. } => crate::workweave::list_workweave_dirs(ctx.primary_path())
            .into_iter()
            .map(|(_, dir)| dir)
            .collect(),
    };
    workweave_dirs
        .into_iter()
        .filter_map(|workweave_dir| {
            let marker = crate::workspace::WorkweaveMarker::read(&workweave_dir).ok()??;
            let project = marker.project().clone();
            if project_scope.is_some_and(|scope| &project != scope) {
                return None;
            }
            let name = crate::workweave::workweave_name_for_path(
                ctx.primary_path(),
                &project,
                &workweave_dir,
            )
            .ok()??;
            let loaded =
                Project::from_dir(&project_dir(&workweave_dir, &project), project.clone()).ok()?;
            Some((WorkweaveId { project, name }, workweave_dir, loaded))
        })
        .collect()
}

/// A member checkout doctor answers for, with the role its files hold
/// ([`crate::settle::role_held`]).
struct MemberSlot {
    workweave: Option<WorkweaveId>,
    checkout: PathBuf,
    repo: RepoPath,
    vcs_type: crate::manifest::VcsType,
    role: Role,
}

/// Every member checkout of the primary when doctor runs there, for the
/// projects in scope, and of each workweave [`workweaves_in_scope`] names. A
/// slot that is absent or a symlink is not a checkout. A clone several
/// projects of the primary list is counted once.
fn member_slots(
    ctx: &crate::workspace::WorkspaceContext,
    projects: &[Project],
    project_scope: Option<&ProjectName>,
) -> Vec<MemberSlot> {
    use crate::workspace::Checkout;

    let mut slots: Vec<MemberSlot> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut push = |workweave: Option<WorkweaveId>, dir: &Path, project: &Project| {
        for (repo, entry) in project.manifest.iter_entries() {
            let checkout = dir.join(repo.as_path());
            if checkout.is_symlink()
                || !checkout.is_dir()
                || crate::workweave::is_dangling_link(&checkout)
                || !seen.insert(checkout.clone())
            {
                continue;
            }
            slots.push(MemberSlot {
                workweave: workweave.clone(),
                role: crate::settle::role_held(&checkout, entry.role),
                checkout,
                repo: repo.clone(),
                vcs_type: entry.vcs_type,
            });
        }
    };
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        for project in projects {
            push(None, ctx.primary_path(), project);
        }
    }
    for (workweave, dir, project) in workweaves_in_scope(ctx, project_scope) {
        push(Some(workweave), &dir, &project);
    }
    slots
}

/// A `lock-pinned-by-name` finding for each entry of a project's `rwv.lock`
/// that holds a tag or branch name rather than a commit id.
fn scan_locks_pinned_by_name(projects: &[Project]) -> Vec<CheckViolation> {
    projects
        .iter()
        .filter_map(|project| project.lock.as_ref().map(|lock| (project, lock)))
        .flat_map(|(project, lock)| {
            lock.entries_pinned_by_name()
                .map(|(repo, version)| CheckViolation::LockPinnedByName {
                    project: project.name.clone(),
                    repo: repo.clone(),
                    version: version.clone(),
                })
        })
        .collect()
}

/// A `pin` finding for each member of a project with a lock whose checkout
/// does not stand at the commit that lock pins for it, and for each member the
/// lock has no entry for.
///
/// Coverage reads the raw lock, because "does the lock name this repo" is a
/// question about the file and not about what resolved: an entry whose repo is
/// absent from disk, or whose revision will not resolve, is dropped from the
/// resolved lock and would read as no entry at all. Freshness reads the
/// resolved lock, so tag-form, branch-form and SHA-form pins compare as the
/// commits they name. A project with no lock file is a separate, unlocked
/// state and earns no finding.
fn scan_pins(
    ctx: &crate::workspace::WorkspaceContext,
    world: &DoctorWorld,
    unjudged: &mut Vec<String>,
) -> Vec<CheckViolation> {
    let input = &world.input;
    let mut live = None;
    let mut found = Vec::new();
    for project in &input.projects {
        let Some(raw) = &project.lock else {
            continue;
        };
        let pin = |repo: &RepoPath, sub_kind: PinKind| CheckViolation::Pin {
            project: project.name.clone(),
            repo: repo.clone(),
            sub_kind,
            locked: None,
            actual: None,
            listed: Vec::new(),
        };
        for repo in project.manifest.iter_repo_paths() {
            if !raw.contains_repo(repo) {
                found.push(pin(repo, PinKind::None));
            }
        }
        for (project_name, repo) in &world.lock_resolve_failures {
            if project_name == &project.name {
                found.push(pin(repo, PinKind::Unreachable));
            }
        }
        let Some(resolved) = input.resolved_locks.get(&project.name) else {
            continue;
        };
        for (repo, entry) in resolved.iter_entries() {
            let Some(head) = input.head_revisions.get(repo) else {
                continue;
            };
            if &entry.version == head {
                continue;
            }
            let dir = world.workspace_dir.join(repo.as_path());
            let vcs = crate::vcs::vcs_for(entry.vcs_type);
            if matches!(vcs.set_back_from(&dir), Ok(Some(_))) {
                continue;
            }
            let live =
                live.get_or_insert_with(|| match crate::pins::live_pins(ctx.primary_path()) {
                    Ok(live) => live,
                    Err(e) => {
                        unjudged.push(format!("the weave's live pins could not be read: {e:#}"));
                        Default::default()
                    }
                });
            let held = live.get(repo).map(Vec::as_slice).unwrap_or_default();
            let standing =
                match crate::pins::standing(vcs.as_ref(), &dir, head, &entry.version, held) {
                    Ok(standing) => standing,
                    Err(e) => {
                        unjudged.push(format!(
                            "{}: its pin could not be judged: {e:#}",
                            dir.display()
                        ));
                        continue;
                    }
                };
            let (sub_kind, listed) = match standing {
                crate::pins::Standing::At => continue,
                crate::pins::Standing::Behind { .. } => (PinKind::Behind, Vec::new()),
                crate::pins::Standing::Diverged => (PinKind::Diverged, Vec::new()),
                crate::pins::Standing::Ahead(_) => (PinKind::Ahead, Vec::new()),
                crate::pins::Standing::Unpinned(beyond) => (
                    PinKind::Unpinned,
                    beyond
                        .unpinned
                        .iter()
                        .map(|c| format!("{} {}", c.id.short(), c.subject))
                        .collect(),
                ),
            };
            found.push(CheckViolation::Pin {
                project: project.name.clone(),
                repo: repo.clone(),
                sub_kind,
                locked: Some(entry.version.clone()),
                actual: Some(head.clone()),
                listed,
            });
        }
    }
    found
}

/// A `role-disagreement` finding for each canonical clone of the primary that
/// the projects in scope list and the projects listing it disagree on, when
/// doctor runs in the primary. Every project listing the clone is named, in
/// scope or not.
fn scan_role_disagreements(
    ctx: &crate::workspace::WorkspaceContext,
    projects: &[Project],
) -> Vec<CheckViolation> {
    if !matches!(ctx.checkout, crate::workspace::Checkout::Primary { .. }) {
        return Vec::new();
    }
    let listed: BTreeSet<&RepoPath> = projects
        .iter()
        .flat_map(|project| project.manifest.iter_repo_paths())
        .collect();
    listed
        .into_iter()
        .filter_map(|repo| {
            let listing = crate::settle::Listing::of(&ctx.primary_path().join(repo.as_path()));
            listing
                .disagreement()
                .map(|roles| CheckViolation::RoleDisagreement {
                    repo: repo.clone(),
                    roles: roles.to_vec(),
                })
        })
        .collect()
}

/// A `history-disagreement` finding for each member the projects in scope
/// list that two projects' live weave branches record different `history`
/// for. Every live branch recording one is named, in scope or not.
fn scan_history_disagreements(
    ctx: &crate::workspace::WorkspaceContext,
    projects: &[Project],
    unjudged: &mut Vec<String>,
) -> Vec<CheckViolation> {
    let listed: BTreeSet<&RepoPath> = projects
        .iter()
        .flat_map(|project| project.manifest.iter_repo_paths())
        .collect();
    let accounting = match crate::alignment::Accounting::scan(ctx.primary_path()) {
        Ok(accounting) => accounting,
        Err(e) => {
            unjudged.push(format!(
                "the history the weave's live branches record could not be read: {e:#}"
            ));
            return Vec::new();
        }
    };
    accounting
        .history_disagreements()
        .into_iter()
        .filter(|(repo, _)| listed.contains(repo))
        .map(|(repo, histories)| CheckViolation::HistoryDisagreement { repo, histories })
        .collect()
}

/// The member-permissions finding for `slot`, when any of its tracked regular
/// files does not hold the permissions its role asks for.
fn member_permissions_finding(slot: &MemberSlot) -> Option<CheckViolation> {
    let vcs = crate::vcs::vcs_for(slot.vcs_type);
    let files = crate::settle::unsettled(vcs.as_ref(), &slot.checkout, slot.role).ok()?;
    (files > 0).then(|| CheckViolation::MemberPermissions {
        workweave: slot.workweave.clone(),
        repo: slot.repo.clone(),
        kind: if slot.role == Role::Reference {
            PermissionsKind::WritableReference
        } else {
            PermissionsKind::ReadOnlyMember
        },
        files,
    })
}

/// `--fix` for `member-permissions`: settle every member slot, and drop the
/// findings of the ones that settled. Runs after the per-finding repairs,
/// which can rewrite a member's files.
fn repair_member_permissions(
    ctx: &crate::workspace::WorkspaceContext,
    projects: &[Project],
    project_scope: Option<&ProjectName>,
    violations: Vec<CheckViolation>,
    fix_errors: &mut Vec<String>,
) -> Vec<CheckViolation> {
    let mut settled: std::collections::HashSet<(Option<WorkweaveId>, RepoPath)> =
        std::collections::HashSet::new();
    for slot in member_slots(ctx, projects, project_scope) {
        let location = match &slot.workweave {
            Some(ww) => format!("{ww}/{}", slot.repo),
            None => slot.repo.to_string(),
        };
        let vcs = crate::vcs::vcs_for(slot.vcs_type);
        match crate::settle::settle(vcs.as_ref(), &slot.checkout, slot.role) {
            Ok(changed) => {
                if changed > 0 {
                    outln!(
                        "[fixed] core: settled {changed} tracked file(s) of {location} for \
                         its role ({role})",
                        role = slot.role,
                    );
                }
                settled.insert((slot.workweave, slot.repo));
            }
            Err(e) => fix_errors.push(format!(
                "member-permissions fix failed for {location}: {e:#}"
            )),
        }
    }
    violations
        .into_iter()
        .filter(|v| match v {
            CheckViolation::MemberPermissions {
                workweave, repo, ..
            } => !settled.contains(&(workweave.clone(), repo.clone())),
            _ => true,
        })
        .collect()
}

/// What stands at a [`MemberGap`]'s slot.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GapSlot {
    Empty,
    Symlink,
    DanglingLink,
}

/// A member of a workweave with no usable worktree there, while the
/// workweave's `rwv.lock` pins it: nothing at its slot, a symlink, or a
/// checkout whose link dangles.
struct MemberGap {
    workweave: WorkweaveId,
    workweave_dir: PathBuf,
    repo: RepoPath,
    vcs_type: crate::manifest::VcsType,
    role: Role,
    recurse_submodules: crate::manifest::SubmoduleRecursion,
    slot: GapSlot,
    /// The revision a worktree created afresh starts at.
    revision: Option<String>,
    /// Whether the canonical clone holds what the worktree needs — its
    /// registration of the slot, the workweave's branch, or `revision` —
    /// so `--fix` can make it without the network.
    local: bool,
}

impl MemberGap {
    fn finding(&self) -> CheckViolation {
        let kind = match (self.slot, self.local) {
            (GapSlot::Empty, true) => DriftKind::Missing,
            (GapSlot::Empty, false) => DriftKind::MissingUnfetched,
            (GapSlot::Symlink, true) => DriftKind::Symlink,
            (GapSlot::Symlink, false) => DriftKind::SymlinkUnfetched,
            (GapSlot::DanglingLink, true) => DriftKind::DanglingLink,
            (GapSlot::DanglingLink, false) => DriftKind::DanglingLinkUnfetched,
        };
        CheckViolation::WorkweaveDrift {
            workweave: Some(self.workweave.clone()),
            kind,
            repo: self.repo.clone(),
        }
    }
}

/// Every [`MemberGap`] in the workweaves doctor answers for: the one it runs
/// in, or, from the primary, each registered workweave of a project in scope.
/// A workweave no registry entry names has no name to mint its branch from;
/// the tree-integrity scan reports it.
///
/// A symlinked member the lock does not pin still counts, at the revision the
/// link showed — the canonical clone's HEAD: every verb refuses while the link
/// stands, `rwv lock` included, so it cannot wait for a pin.
fn scan_member_gaps(
    ctx: &crate::workspace::WorkspaceContext,
    project_scope: Option<&ProjectName>,
) -> Vec<MemberGap> {
    let mut gaps = Vec::new();
    for (workweave, workweave_dir, loaded) in workweaves_in_scope(ctx, project_scope) {
        let branch = crate::vcs::EphemeralRefName::mint(&workweave.flat()).to_raw();
        for (repo, entry) in loaded.manifest.iter_entries() {
            let slot_path = workweave_dir.join(repo.as_path());
            let slot = if crate::workweave::is_symlinked_member(&slot_path) {
                GapSlot::Symlink
            } else if crate::workweave::is_dangling_link(&slot_path) {
                if !ctx.primary_path().join(repo.as_path()).exists() {
                    continue;
                }
                GapSlot::DanglingLink
            } else if slot_path.exists() {
                continue;
            } else {
                GapSlot::Empty
            };
            let symlinked = slot == GapSlot::Symlink;
            let vcs = crate::vcs::vcs_for(entry.vcs_type);
            let canonical = ctx.primary_path().join(repo.as_path());
            let canonical_is_repo = vcs.is_repo(&canonical);
            let pinned = loaded
                .lock
                .as_ref()
                .and_then(|lock| lock.get_entry(repo))
                .map(|e| e.version.as_str().to_owned());
            let revision = match pinned {
                Some(rev) => Some(rev),
                None if symlinked => canonical_is_repo
                    .then(|| vcs.head_revision(&canonical).ok())
                    .flatten()
                    .map(|rev| rev.id().as_str().to_owned()),
                None => continue,
            };
            let local = canonical_is_repo
                && (revision
                    .as_ref()
                    .is_some_and(|rev| vcs.resolve_revision(&canonical, rev).is_ok())
                    || vcs
                        .resolve_local_branch_tip(&canonical, &branch)
                        .is_ok_and(|tip| tip.is_some())
                    || (slot == GapSlot::DanglingLink
                        && vcs
                            .worktree_registration(&canonical, &slot_path)
                            .is_ok_and(|r| r.is_some())));
            gaps.push(MemberGap {
                workweave: workweave.clone(),
                workweave_dir: workweave_dir.clone(),
                repo: repo.clone(),
                vcs_type: entry.vcs_type,
                role: entry.role,
                recurse_submodules: entry.recurse_submodules,
                slot,
                revision,
                local,
            });
        }
    }
    gaps
}

/// An [`DriftKind::Extra`] finding for each checkout a project's `rwv.lock`
/// names and its manifest no longer does: the canonical clone, when doctor
/// runs in the primary and no other project's manifest lists it, and the
/// worktree in each workweave [`workweaves_in_scope`] names. A path neither
/// file names is not this scan's: the orphan walk reports it under `--all`.
fn scan_leftover_checkouts(
    ctx: &crate::workspace::WorkspaceContext,
    project_scope: Option<&ProjectName>,
    projects: &[Project],
) -> Vec<CheckViolation> {
    let mut found = Vec::new();
    let leftovers = |project: &Project, root: &Path| -> Vec<RepoPath> {
        let Some(lock) = project.lock.as_ref() else {
            return Vec::new();
        };
        lock.iter_entries()
            .filter(|(repo, _)| project.manifest.get_entry(repo).is_none())
            .filter(|(repo, entry)| {
                crate::vcs::vcs_for(entry.vcs_type).is_repo(&root.join(repo.as_path()))
            })
            .map(|(repo, _)| repo.clone())
            .collect()
    };
    let drift = |workweave: Option<WorkweaveId>, repo: RepoPath| CheckViolation::WorkweaveDrift {
        workweave,
        kind: DriftKind::Extra,
        repo,
    };

    if matches!(ctx.checkout, crate::workspace::Checkout::Primary { .. }) {
        let primary_manifests: Vec<(ProjectName, Manifest)> =
            crate::workspace::discover_projects(ctx.primary_path())
                .into_iter()
                .filter_map(|name| {
                    let dir = project_dir(ctx.primary_path(), &name);
                    Manifest::from_path(&dir.join(Manifest::FILE_NAME))
                        .ok()
                        .map(|manifest| (name, manifest))
                })
                .collect();
        for project in projects {
            for repo in leftovers(project, ctx.primary_path()) {
                let listed_elsewhere = primary_manifests
                    .iter()
                    .any(|(name, m)| name != &project.name && m.get_entry(&repo).is_some());
                if !listed_elsewhere {
                    found.push(drift(None, repo));
                }
            }
        }
    }
    for (workweave, workweave_dir, project) in workweaves_in_scope(ctx, project_scope) {
        for repo in leftovers(&project, &workweave_dir) {
            found.push(drift(Some(workweave.clone()), repo));
        }
    }
    found
}

/// `--fix` for the [`MemberGap`]s the canonical clone can close on its own,
/// through [`crate::workweave::materialize_member`]. One it cannot is left
/// for `rwv update`, which doctor does not run — except a dangling checkout
/// under `abandon_lost_pins`, which is relinked onto the workweave's branch
/// created at the canonical clone's HEAD, its pinned commits given up.
fn repair_member_gaps(
    ctx: &crate::workspace::WorkspaceContext,
    project_scope: Option<&ProjectName>,
    abandon_lost_pins: Option<crate::cli::consent::AbandonLostPinsConsent>,
    fix_errors: &mut Vec<String>,
) {
    for gap in scan_member_gaps(ctx, project_scope) {
        let vcs = crate::vcs::vcs_for(gap.vcs_type);
        let abandoned = match (gap.local, gap.slot, abandon_lost_pins) {
            (true, _, _) => None,
            (false, GapSlot::DanglingLink, Some(_)) => {
                match vcs.head_revision(&ctx.primary_path().join(gap.repo.as_path())) {
                    Ok(head) => Some(head),
                    Err(e) => {
                        fix_errors.push(format!(
                            "workweave-drift fix failed for {}/{}: the canonical clone's \
                             HEAD is unreadable: {e:#}",
                            gap.workweave, gap.repo
                        ));
                        continue;
                    }
                }
            }
            (false, _, _) => continue,
        };
        let revision = match &abandoned {
            Some(head) => head.id().as_str(),
            None => gap.revision.as_deref().unwrap_or_default(),
        };
        match crate::workweave::materialize_member(
            vcs.as_ref(),
            ctx.primary_path(),
            &gap.workweave_dir,
            &gap.workweave.project,
            &gap.workweave.name,
            &gap.repo,
            gap.role,
            gap.recurse_submodules,
            revision,
            crate::workweave::BranchPolicy::Restore,
        ) {
            Ok(born) if abandoned.is_some() => outln!(
                "[fixed] core: relinked the worktree for {repo} in workweave {ww}: {how}, the \
                 canonical clone's HEAD; its pinned revision {pin} is in no clone and was given \
                 up, and the checkout's files stand as edits against that HEAD",
                repo = gap.repo,
                ww = gap.workweave,
                how = born.summary(),
                pin = gap
                    .revision
                    .as_deref()
                    .map_or("(none)", crate::vcs::short_revision),
            ),
            Ok(born) if gap.slot == GapSlot::DanglingLink => outln!(
                "[fixed] core: relinked the worktree for {repo} in workweave {ww}: {how}",
                repo = gap.repo,
                ww = gap.workweave,
                how = born.summary(),
            ),
            Ok(born) => outln!(
                "[fixed] core: created the worktree for {repo} in workweave {ww}: {how}{was}{files}",
                repo = gap.repo,
                ww = gap.workweave,
                how = born.summary(),
                was = if gap.slot == GapSlot::Symlink {
                    ", replacing its symlink"
                } else {
                    ""
                },
                files = crate::share::files_note(born.files),
            ),
            Err(e) => fix_errors.push(format!(
                "workweave-drift fix failed for {}/{}: {e:#}",
                gap.workweave, gap.repo
            )),
        }
    }
}

/// `--fix` for `standalone-in-workweave` in the workweaves
/// [`workweaves_in_scope`] names: replace each member that is a full clone
/// with a worktree of the canonical clone, or report why it was left.
fn repair_standalone_members(
    ctx: &crate::workspace::WorkspaceContext,
    project_scope: Option<&ProjectName>,
    fix_errors: &mut Vec<String>,
) {
    for (workweave, workweave_dir, loaded) in workweaves_in_scope(ctx, project_scope) {
        for (repo, entry) in loaded.manifest.iter_entries() {
            let slot = workweave_dir.join(repo.as_path());
            if crate::workweave::is_symlinked_member(&slot) {
                continue;
            }
            let vcs = crate::vcs::vcs_for(entry.vcs_type);
            let standalone = vcs
                .store_of(&slot)
                .is_some_and(|store| crate::workweave::is_own_store(&store, &slot));
            if !standalone {
                continue;
            }
            match crate::workweave::replace_standalone_member(
                vcs.as_ref(),
                ctx.primary_path(),
                &workweave_dir,
                &workweave.project,
                &workweave.name,
                repo,
                entry.role,
                entry.recurse_submodules,
            ) {
                Ok(born) => outln!(
                    "[fixed] core: replaced the full clone of {repo} in workweave {workweave} \
                     with a worktree of the canonical clone at {at}{files}",
                    at = born.at.id().short(),
                    files = crate::share::files_note(born.files),
                ),
                Err(e) => fix_errors.push(format!(
                    "clone-topology fix left {workweave}/{repo} standing: {e:#}"
                )),
            }
        }
    }
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
    /// The workweave the repo lives in; `None` for repos in the primary
    /// weave.
    pub workweave: Option<WorkweaveId>,
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
///   runs [`crate::vcs::Vcs::prune_stale_worktree_registration`]; this scanner
///   only reports.
/// - **stale-op-state**: an in-tree `.rwv-op` file (any one). Reported
///   with its `started_at` field intact. **Never auto-fixed** — another
///   terminal may be mid-conflict-resolution and rwv has no daemon to
///   know.
/// - **orphaned-savepoint**: a `refs/rwv/pre-op/<op-id>` ref whose
///   `op_id` is not present in any live `.rwv-op` file inside
///   `op_state_targets`. Classified as
///   [`OrphanedSavepointKind::Redundant`] when a branch reaches every tip
///   filed under the key, and [`OrphanedSavepointKind::Live`] otherwise.
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
    let mut live_savepoints: std::collections::HashSet<SavepointKey> =
        std::collections::HashSet::new();
    let mut every_savepoint_live = false;
    for target in op_state_targets {
        match crate::op_state::read_owner_header(&target.workspace_dir) {
            Ok(None) => {}
            Ok(Some(state)) => {
                violations.push(CheckViolation::StaleOpState {
                    workspace_dir: target.workspace_dir.clone(),
                    verb: state.verb,
                    started_at: state.started_at.clone(),
                });
                live_savepoints.extend(state.savepoint_keys());
            }
            Err(e) => {
                violations.push(CheckViolation::UnreadableOpRecord {
                    workspace_dir: target.workspace_dir.clone(),
                    error: format!("{e:#}"),
                });
                match crate::op_state::read_owner_header_of_any_schema(&target.workspace_dir) {
                    Some(state) => live_savepoints.extend(state.savepoint_keys()),
                    None => every_savepoint_live = true,
                }
            }
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
                    DeadOpLeaseKind::OwnerOpIdMismatch {
                        owner_op_id: owner_op_id.to_string(),
                    }
                }
            };
            violations.push(CheckViolation::DeadOpLease {
                workspace_dir: dead.workspace_dir,
                op_id: dead.op_id.to_string(),
                recorded_owner: dead.recorded_owner,
                sub_kind,
                created_at: dead.created_at,
            });
        }
    }

    // Phase 2: per-repo checks (stale worktree registrations + orphaned
    // savepoints).
    // Refs live in the store every worktree of a clone shares, so one ref is
    // seen once per checkout of it.
    let mut reported_refs: std::collections::HashSet<(PathBuf, String)> =
        std::collections::HashSet::new();
    for repo in repos {
        // Skip repos that are not vcs-managed (the manifest may list a
        // path that hasn't been cloned yet).
        if !vcs.is_repo(&repo.abs) {
            continue;
        }

        // Stale worktree registrations.
        if let Ok(stale_paths) = vcs.list_stale_worktree_registrations(&repo.abs) {
            for worktree_path in stale_paths {
                let sub_kind = if worktree_path.symlink_metadata().is_ok() {
                    RegistrationDirectoryKind::DirectoryStands
                } else {
                    RegistrationDirectoryKind::DirectoryGone
                };
                violations.push(CheckViolation::StaleWorktreeRegistration {
                    workweave: repo.workweave.clone(),
                    repo: repo.repo.clone(),
                    worktree_path,
                    sub_kind,
                });
            }
        }

        if let Some(store) = vcs.store_of(&repo.abs) {
            if reported_refs.insert((store, String::new())) {
                violations.extend(
                    crate::landing_claim::unreachable_claims(vcs, &repo.abs)
                        .into_iter()
                        .map(|claim| CheckViolation::ClaimOwnerUnreachable {
                            claim_dir: claim.dir,
                            op_id: claim.op.to_string(),
                            recorded_owner: claim.owner,
                        }),
                );
                violations.extend(
                    crate::landing_claim::unreadable_claims(vcs, &repo.abs)
                        .into_iter()
                        .map(|claim| CheckViolation::ClaimUnreadable {
                            file: claim.file,
                            seen: claim.seen,
                        }),
                );
                violations.extend(
                    crate::landing_claim::unopenable_claims(vcs, &repo.abs)
                        .into_iter()
                        .map(|claim| CheckViolation::ClaimUnopenable {
                            file: claim.file,
                            error: claim.error,
                        }),
                );
            }
        }

        let listing = match vcs.list_savepoints(&repo.abs) {
            Ok(listing) => listing,
            Err(_) => continue, // can't enumerate → leave repo alone
        };
        let store = vcs.store_of(&repo.abs).unwrap_or_else(|| repo.abs.clone());
        for ref_name in listing.unrecognized {
            if !reported_refs.insert((store.clone(), ref_name.clone())) {
                continue;
            }
            violations.push(CheckViolation::UnrecognizedSavepointRef {
                workweave: repo.workweave.clone(),
                repo: repo.repo.clone(),
                holder: repo.abs.clone(),
                ref_name,
            });
        }
        for key in listing.keys {
            // Skip savepoints whose op-id matches an in-flight `.rwv-op`.
            // The owning sync may still need to roll back.
            if every_savepoint_live || live_savepoints.contains(&key) {
                continue;
            }
            let sub_kind = match unreached_savepoint_tips(vcs, &repo.abs, &key) {
                Ok(unreached) if unreached.is_empty() => OrphanedSavepointKind::Redundant,
                Ok(_) | Err(_) => OrphanedSavepointKind::Live,
            };
            violations.push(CheckViolation::OrphanedSavepoint {
                workweave: repo.workweave.clone(),
                repo: repo.repo.clone(),
                key,
                sub_kind,
            });
        }
    }

    violations
}

/// Each tip filed under `key` in `repo`'s store, the checkout's and every
/// branch's, that no branch reaches, with what a backup of it backs up.
fn unreached_savepoint_tips(
    vcs: &dyn crate::vcs::Vcs,
    repo: &Path,
    key: &SavepointKey,
) -> Result<Vec<(crate::vcs::BackupSource, ResolvedRevisionId)>, crate::vcs::VcsError> {
    let mut tips: Vec<(crate::vcs::BackupSource, ResolvedRevisionId)> = vcs
        .resolve_savepoint(repo, key)
        .map(|tip| (crate::vcs::BackupSource::DetachedHead, tip))
        .into_iter()
        .collect();
    for saved in vcs.ref_savepoints(repo, key)? {
        tips.push((
            crate::vcs::BackupSource::Branch(saved.branch),
            ResolvedRevisionId::from(saved.at),
        ));
    }
    let mut unreached = Vec::new();
    for (source, tip) in tips {
        if !vcs
            .reached_by_no_branch(repo, &tip, crate::vcs::RemoteTracking::Counted)?
            .is_empty()
        {
            unreached.push((source, tip));
        }
    }
    Ok(unreached)
}

/// Apply the `--fix` for a single state-hygiene violation.
///
/// The accepted `--fix` paths are deliberately narrow:
///
/// - [`CheckViolation::StaleWorktreeRegistration`] with
///   [`RegistrationDirectoryKind::DirectoryGone`] →
///   [`crate::vcs::Vcs::prune_stale_worktree_registration`] of its path in
///   the registering repo. Information-preserving by construction: the only
///   state dropped is a pointer to a directory that does not exist, and a
///   locked registration, or one whose directory has come back, is kept.
/// - [`CheckViolation::OrphanedSavepoint`], either kind → each tip filed
///   under the key that no branch reaches when the fix runs is kept as a
///   backup, then every savepoint under the key is dropped. This is the
///   only route from a savepoint to a backup; nothing reads a savepoint as
///   one.
/// - [`CheckViolation::DeadOpLease`] → [`crate::op_state::fix_dead_lease`]
///   removes the dangling `.rwv-op-lease`. Safe by construction: the
///   classification proved the lease is paired with no live owner record
///   (either the owner file is gone or the owner is now on a different
///   op), so no in-flight op can be disrupted. Structural — no wall-clock
///   input.
///
/// All other variants (`StaleOpState`) return `Ok(false)`: not
/// auto-fixable, the caller should report only.
/// Returns `Ok(true)` when a fix was actually applied; `Ok(false)` when
/// the violation isn't in the `--fix` set; `Err` only when the
/// underlying VCS operation itself errored.
pub fn fix_state_hygiene(
    vcs: &dyn crate::vcs::Vcs,
    violation: &CheckViolation,
    repo_abs: &Path,
) -> anyhow::Result<bool> {
    match violation {
        CheckViolation::StaleWorktreeRegistration {
            worktree_path,
            sub_kind: RegistrationDirectoryKind::DirectoryGone,
            ..
        } => {
            let outcome = vcs
                .prune_stale_worktree_registration(repo_abs, worktree_path)
                .with_context(|| format!("worktree prune failed in {}", repo_abs.display()))?;
            Ok(outcome == crate::vcs::RegistrationPrune::Pruned)
        }
        CheckViolation::OrphanedSavepoint { key, .. } => {
            let unreached = unreached_savepoint_tips(vcs, repo_abs, key).with_context(|| {
                format!(
                    "could not read the savepoints filed under {key} in {}",
                    repo_abs.display()
                )
            })?;
            for (source, tip) in &unreached {
                vcs.create_backup(repo_abs, source, tip, None)
                    .with_context(|| {
                        format!(
                            "could not keep the savepoint {key} as a backup in {}",
                            repo_abs.display()
                        )
                    })?;
            }
            vcs.drop_savepoint(repo_abs, key);
            Ok(true)
        }
        CheckViolation::DeadOpLease { workspace_dir, .. } => {
            crate::op_state::fix_dead_lease(workspace_dir);
            Ok(true)
        }
        // Stale op-state and every other variant are not in the `--fix` set.
        _ => Ok(false),
    }
}

/// `$schema` URL embedded in `rwv doctor --json` output. Points at the
/// committed schema artifact in the main branch, regenerated by
/// `cargo run --example generate-explain`; CI fails on drift.
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
    /// Resolved workspace coordinates (weave root, optional workweave
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
    /// Manifest entries whose checkout stands but whose worktree link
    /// dangles: on disk, though git cannot open them. workweave-drift and
    /// missing-canonical-clone report them, not dangling-reference.
    pub dangling_links: BTreeSet<RepoPath>,
    /// Git repos found on disk under a top-level directory
    /// `scannable_registry_segments` does not cover — see
    /// [`crate::workspace::scan_unknown_segment_repos`]. Always
    /// evaluated, unlike `repos_on_disk`'s orphan comparison: which segments
    /// are unknown is already a fact about every project's manifest under
    /// the weave root, not about which projects this invocation loaded.
    pub unknown_segment_repos: Vec<RepoPath>,
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
    /// For a manifest entry absent from disk, keyed by its project and path:
    /// the path its URL places it at, when a clone of the same remote stands
    /// there and no entry of the project names that path. Such a clone is
    /// accounted for by its entry, not orphaned.
    pub clones_at_placement: std::collections::HashMap<(ProjectName, RepoPath), RepoPath>,
    /// When `false`, the orphan-clone check is skipped. Orphan detection is
    /// inherently weave-wide (a repo is only "orphaned" if it belongs to *no*
    /// project), so running it in single-project mode would produce false
    /// positives for repos that belong to other projects. Set to `true` only
    /// when all projects have been loaded into `projects` (i.e. `--all` mode).
    pub check_orphans: bool,
}

/// The path `entry`'s URL places it at, when a clone of the same remote stands
/// there in `workspace_dir` and no entry of `manifest` names the path — where
/// `repo`'s checkout went if it was moved to its placement before the entry was
/// re-keyed.
fn clone_at_placement(
    workspace_dir: &Path,
    manifest: &Manifest,
    repo: &RepoPath,
    entry: &crate::manifest::RepoEntry,
) -> Option<RepoPath> {
    use crate::registry::{recorded_remotes, resolve_to_clone_info, RecordedRemote};
    let placed = crate::registry::placement(&entry.url)?;
    if &placed == repo || manifest.get_entry(&placed).is_some() {
        return None;
    }
    let vcs = crate::vcs::vcs_for(entry.vcs_type);
    let clone = workspace_dir.join(placed.as_path());
    if !vcs.is_repo(&clone) {
        return None;
    }
    let declared = resolve_to_clone_info(&entry.url).ok()?.url;
    let recorded = recorded_remotes(vcs.as_ref(), &clone).ok()?;
    recorded
        .iter()
        .any(|(_, remote)| matches!(remote, RecordedRemote::Url(actual) if declared.names_same_remote(actual)))
        .then_some(placed)
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
        let accounted: BTreeSet<&RepoPath> = input.clones_at_placement.values().collect();
        for repo_path in &input.repos_on_disk {
            if !input.known_repos.contains(repo_path) && !accounted.contains(repo_path) {
                violations.push(CheckViolation::OrphanedClone {
                    path: repo_path.clone(),
                });
            }
        }
    }

    // Unknown-segment clones: every entry is already excluded from
    // `repos_on_disk`'s own walk by construction (its top-level directory
    // is not one any loaded or unloaded project names), so unlike
    // `OrphanedClone` above this needs no `known_repos` comparison and no
    // `check_orphans` gate.
    for repo_path in &input.unknown_segment_repos {
        violations.push(CheckViolation::UnknownSegmentClone {
            path: repo_path.clone(),
        });
    }

    // Per-project checks
    for project in &input.projects {
        for (repo_path, entry) in project.manifest.iter_entries() {
            // Dangling reference: in manifest but not on disk.
            // Reference repos are allowed to be missing (e.g. after fetch --no-reference).
            // A url no clone can use is reported as unresolvable-url alone: fetch
            // cannot repair it until the url is respelled.
            if !input.repos_on_disk.contains(repo_path)
                && !input.dangling_links.contains(repo_path)
                && entry.role != Role::Reference
                && crate::registry::resolve_to_clone_info(&entry.url).is_ok()
            {
                violations.push(CheckViolation::DanglingReference {
                    project: project.name.clone(),
                    repo: repo_path.clone(),
                    clone_at_placement: input
                        .clones_at_placement
                        .get(&(project.name.clone(), repo_path.clone()))
                        .cloned(),
                });
            }
        }
    }

    violations
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
///   `dead-op-lease`, redundant orphaned savepoints, live orphaned savepoints
///   (kept as backups).
///
/// Everything else stays itemized: a finding outside these classes is a
/// distinct fact the operator has not seen before, and collapsing it would
/// bury it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CollapsedClass {
    StaleRegistryEntry,
    StaleWorktreeRegistration,
    DeadOpLease,
    RedundantSavepoint,
    LiveSavepoint,
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
            CheckViolation::StaleWorktreeRegistration {
                sub_kind: RegistrationDirectoryKind::DirectoryGone,
                ..
            } => Some(Self::StaleWorktreeRegistration),
            CheckViolation::DeadOpLease { .. } => Some(Self::DeadOpLease),
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
            | Self::DeadOpLease
            | Self::RedundantSavepoint
            | Self::LiveSavepoint => true,
        }
    }

    /// The one text line the class renders: count, class, remedy, and where
    /// the per-item records are.
    fn issue(self, n: usize, severity: crate::integration::Severity) -> Issue {
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
            Self::DeadOpLease => format!(
                "{n} dead-op-lease finding{s} — lease file{s} whose recorded \
                 owner holds no matching op; `rwv doctor --fix` clears them"
            ),
            Self::RedundantSavepoint => format!(
                "{n} redundant orphaned-savepoint finding{s} — savepoint tip{s} \
                 already anchored by a live branch; `rwv doctor --fix` drops them"
            ),
            Self::LiveSavepoint => format!(
                "{n} live orphaned-savepoint finding{s} — savepoint{s} holding \
                 commits no live ref anchors; `rwv doctor --fix` keeps them as \
                 backups, which `rwv backup list` shows"
            ),
        };
        let message = format!("{body}; per-item detail: `rwv doctor --json`");
        Issue {
            kind: IssueKind::CoreFinding,
            source: IssueSource::Core,
            severity,
            message: message.into(),
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
fn violations_to_issues(
    violations: Vec<CheckViolation>,
    token_of: impl Fn(&CheckViolation) -> String,
) -> Vec<ReportLine> {
    use crate::integration::Severity;
    let mut counts: std::collections::BTreeMap<CollapsedClass, (usize, Severity, String)> =
        std::collections::BTreeMap::new();
    let mut itemized = Vec::new();
    for v in violations {
        match CollapsedClass::of(&v) {
            Some(class) => {
                let (n, severity, _) = counts
                    .entry(class)
                    .or_insert_with(|| (0, Severity::Warning, token_of(&v)));
                *n += 1;
                if v.severity() == Severity::Error {
                    *severity = Severity::Error;
                }
            }
            None => itemized.push(v),
        }
    }
    let mut lines = itemized_violations_to_issues(itemized, token_of);
    lines.extend(
        counts
            .into_iter()
            .map(|(class, (n, severity, token))| ReportLine {
                token,
                issue: class.issue(n, severity),
            }),
    );
    lines
}

/// The per-item rendering behind [`violations_to_issues`] — every violation
/// here produces its own line (or is dropped by the one deliberate
/// json-only carve-out below).
fn itemized_violations_to_issues(
    violations: Vec<CheckViolation>,
    token_of: impl Fn(&CheckViolation) -> String,
) -> Vec<ReportLine> {
    violations
        .into_iter()
        .filter_map(|v| {
            let token = token_of(&v);
            v.into_text_issue().map(|issue| ReportLine { token, issue })
        })
        .collect()
}

impl CheckViolation {
    /// This finding as its own line of the text report, or `None` for the one
    /// kind only `--json` carries.
    pub fn into_text_issue(self) -> Option<Issue> {
        let v = self;
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
        let severity = v.severity();
        let message = match v {
            CheckViolation::ConfusableSiblings {
                parent,
                first,
                second,
            } => crate::workspace::confusable_warning(&crate::workspace::ConfusableSiblings {
                parent: parent.clone(),
                first: first.clone(),
                second: second.clone(),
            }),
            CheckViolation::OrphanedClone { path } => format!(
                "orphaned clone: {path} — not listed in any project's rwv.toml; to \
                     register it, move it to the path its remote's URL places it at under \
                     the primary weave root, unless it already sits there, and run \
                     `rwv member add --adopt` with that path; or remove the directory manually"
            ),
            CheckViolation::UnknownSegmentClone { path } => format!(
                "unknown-segment clone: {path} — sits under a top-level directory no \
                     built-in registry and no project's rwv.toml names, so no scan compares \
                     it against anything; to register it, move it to the path its remote's \
                     URL places it at under the primary weave root, unless it already sits \
                     there, and run `rwv member add --adopt` with that path; or remove the directory \
                     manually"
            ),
            // `doctor --fix` intentionally does NOT auto-clone: network
            // side effects stay behind an explicit verb.
            CheckViolation::DanglingReference {
                project,
                repo,
                clone_at_placement: None,
            } => format!(
                "dangling reference in {project}: {repo} — \
                     listed in rwv.toml but not cloned on disk; \
                     run `rwv update` from the workspace to re-materialize \
                     missing manifest members, then re-run `rwv doctor` to verify"
            ),
            CheckViolation::DanglingReference {
                project,
                repo,
                clone_at_placement: Some(placed),
            } => format!(
                "dangling reference in {project}: {repo} — listed in rwv.toml but not \
                     cloned on disk, while a clone of its URL stands at `{placed}`, where \
                     that URL places it; re-key the entry in rwv.toml from `{repo}` to \
                     `{placed}`, run `rwv lock`, and pin it with `rwv member pin {placed} \
                     <rev> --commit` (`rwv update` would clone it a second time at `{repo}`)"
            ),
            CheckViolation::MemberPermissions {
                workweave,
                repo,
                kind,
                files,
            } => {
                let location = match workweave {
                    Some(ww) => format!("{ww}/{repo}"),
                    None => format!("{repo}"),
                };
                let detail = match kind {
                    PermissionsKind::WritableReference => format!(
                        "{files} tracked file(s) of reference member {location} are \
                             writable; `rwv doctor --fix` removes their write bits"
                    ),
                    PermissionsKind::ReadOnlyMember => format!(
                        "{files} tracked file(s) of {location} are read-only, though it \
                             is not a reference member; `rwv doctor --fix` restores the \
                             owner's write bit, first giving a file shared with another \
                             workspace a private copy"
                    ),
                };
                format!("member permissions: {detail}")
            }
            CheckViolation::HistoryDisagreement { repo, histories } => format!(
                "history disagreement: {repo} — the projects listing this member record \
                     different `history` for it ({}); each project's landings follow its own \
                     setting, so the member's branches take both shapes; give every project \
                     the same setting with `rwv branch set-history --project <name> <h> --repo \
                     {repo}` in a workspace on each branch",
                histories
                    .iter()
                    .map(|recorded| format!(
                        "{} {}: {}",
                        recorded.project, recorded.branch, recorded.history
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            CheckViolation::RoleDisagreement { repo, roles } => format!(
                "role disagreement: {repo} — the projects listing this clone declare \
                     different roles ({}); a clone several projects share is read-only \
                     only when every one of them declares `reference`, so its files stay \
                     writable; give every project the same role with \
                     `rwv member set-role --project <name> {repo} <role>`",
                roles
                    .iter()
                    .map(|(project, role)| format!("{project}: {role}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            CheckViolation::Pin {
                project,
                repo,
                sub_kind,
                locked,
                actual,
                listed,
            } => {
                let compared = match (locked, actual) {
                    (Some(locked), Some(actual)) => format!(
                        "locked={} actual={}",
                        locked.short_display(),
                        actual.short_display()
                    ),
                    _ => String::new(),
                };
                match sub_kind {
                    PinKind::None => format!(
                        "{project}: {repo} has no rwv.lock entry; pin it with `rwv member \
                         pin {repo} <rev> --commit`"
                    ),
                    PinKind::Unreachable => format!(
                        "{project}: lock references unknown revision for {repo}; `rwv fetch` \
                         brings in the history that may hold it, or pin another commit with \
                         `rwv member pin {repo} <rev>`"
                    ),
                    PinKind::Behind => format!(
                        "{project}: {repo} {compared}; the checkout is behind its pin: `rwv \
                         update` moves it to the pin, or `rwv member pin {repo} HEAD` pins the \
                         checkout's commit by name"
                    ),
                    PinKind::Ahead => format!(
                        "{project}: {repo} {compared}; the checkout is ahead of its pin: \
                         `rwv lock` records its commit"
                    ),
                    PinKind::Unpinned => format!(
                        "{project}: {repo} {compared}; the checkout holds commits nothing \
                         else holds beyond its pin ({}): `rwv lock --commit` records them",
                        listed.join("; ")
                    ),
                    PinKind::Diverged => format!(
                        "{project}: {repo} {compared}; the checkout has diverged from its \
                         pin: `rwv member pin {repo} HEAD` pins its commit by name, or `rwv \
                         update` moves it to the pin"
                    ),
                }
            }
            CheckViolation::WorkweaveDrift {
                workweave,
                kind,
                repo,
            } => {
                let fetch_then_fix = format!(
                    " — git cannot open it, and the canonical clone holds neither the \
                         workweave's branch nor the revision rwv.lock pins. If that revision \
                         is on the remote, run `rwv update --project {}` at the primary weave, \
                         then `rwv doctor --fix`, which relinks it onto that revision. If it is \
                         in no clone — its commits went with a deleted clone — \
                         `rwv doctor --fix --abandon-lost-pins` relinks it onto the \
                         workweave's branch created at the canonical clone's HEAD, keeping \
                         every file, so those commits' changes stand as edits",
                    workweave
                        .as_ref()
                        .map_or_else(|| "<project>".to_owned(), |ww| ww.project.to_string())
                );
                let (kind_str, hint) = match kind {
                    DriftKind::Missing => (
                        "missing worktree",
                        " — the canonical clone holds the workweave's branch or the \
                             revision rwv.lock pins; `rwv doctor --fix` creates the worktree \
                             on that branch at its own tip, or at that revision where the \
                             branch is gone",
                    ),
                    DriftKind::DanglingLink => (
                        "worktree link to a missing git directory at",
                        " — git cannot open the checkout, though its files stand; \
                             `rwv doctor --fix` relinks it through the registration the \
                             canonical clone kept, else onto the workweave's branch at its \
                             own tip, else onto that branch created at the revision rwv.lock \
                             pins, leaving every file in place",
                    ),
                    DriftKind::DanglingLinkUnfetched => (
                        "worktree link to a missing git directory at",
                        fetch_then_fix.as_str(),
                    ),
                    DriftKind::MissingUnfetched => (
                        "missing worktree",
                        " — the canonical clone does not hold the revision rwv.lock \
                             pins, or is not on disk; `rwv update` run in that workweave \
                             fetches the revision into it, cloning it first if need be, and \
                             creates the worktree",
                    ),
                    DriftKind::Symlink => (
                        "symlink instead of a worktree at",
                        " — rwv no longer operates on a symlinked member; \
                             `rwv doctor --fix` replaces the link with a worktree at the \
                             revision rwv.lock pins, leaving what it points at untouched",
                    ),
                    DriftKind::SymlinkUnfetched => (
                        "symlink instead of a worktree at",
                        " — rwv no longer operates on a symlinked member, and the \
                             canonical clone does not hold the revision rwv.lock pins, or is \
                             not on disk; `rwv update` run in that workweave fetches the \
                             revision into it, cloning it first if need be, and replaces the \
                             link with a worktree",
                    ),
                    DriftKind::Extra => (
                        "leftover checkout of",
                        " — rwv.lock still names it and rwv.toml no longer does; \
                             `rwv member remove <path> --delete` deletes the checkout, and \
                             `rwv member add <url>` keeps it as a member",
                    ),
                };
                format!(
                    "workweave drift in {}: {kind_str} {repo}{hint}",
                    workweave
                        .as_ref()
                        .map_or_else(|| "the primary".to_owned(), WorkweaveId::to_string)
                )
            }
            CheckViolation::RecordDrift {
                workweave,
                repo,
                kind,
            } => {
                let location = match workweave {
                    Some(ww) => format!("{ww}/{repo}"),
                    None => format!("{repo}"),
                };
                let detail = match kind {
                    RecordDriftKind::SafeToFix => "recorded state stale (run `rwv doctor --fix` to reset)",
                    RecordDriftKind::LiveRecorded => {
                        "live changes are marked for commit (once `rwv doctor --fix` has \
                             refreshed what beside them is stale, commit them if they are yours, \
                             or set them aside; not auto-fixed)"
                    }
                };
                format!("{location}: {detail}")
            }
            CheckViolation::MissingLockMergeDriver { project, sub_kind } => format!(
                "{project}: lock merge driver {}: {} (run `rwv doctor --fix` to set it up)",
                sub_kind.state().row().name,
                sub_kind.state().row().means
            ),
            CheckViolation::LockMergeDriverUnreadable { project, error } => {
                format!("{project}: failed to read the lock merge driver setup: {error}")
            }
            CheckViolation::HeadUnreadable { repo, error } => {
                format!("{repo}: HEAD unreadable ({error})")
            }
            CheckViolation::ProjectsDirUnreadable { path, error } => format!(
                "{}: projects directory unreadable ({error}); every project under it \
                     is invisible to this scan",
                path.display()
            ),
            CheckViolation::ProjectlessDir { dir } => format!(
                "{}: directory under projects/ holds no {} at any depth, so it is \
                     not a project and rwv lists nothing for it; write a {} there \
                     (`[repositories]` alone is enough) or remove the directory",
                dir.display(),
                Manifest::FILE_NAME,
                Manifest::FILE_NAME,
            ),
            CheckViolation::UnnameableProject {
                dir,
                derived,
                error,
            } => format!(
                "{}: holds a {} but `{derived}` is not a usable project name \
                     ({error}), so no verb can address it; rename the directory to a \
                     name rwv accepts",
                dir.display(),
                Manifest::FILE_NAME,
            ),
            CheckViolation::LockPinnedByName {
                project,
                repo,
                version,
            } => format!(
                "{project}: {}",
                crate::lock::pinned_by_name_warning(&repo, &version)
            ),
            CheckViolation::LegacyManifestFormat {
                project,
                legacy_path,
            } => format!(
                "{project}: {}",
                Manifest::legacy_format_refusal(legacy_path.as_path())
            ),
            CheckViolation::LegacyWorkweaveMarker { marker_path, .. } => format!(
                "{} is a legacy (YAML) workweave marker; \
                     run `rwv doctor --fix` to migrate",
                marker_path.display()
            ),
            CheckViolation::LegacyWorkweaveIndex { index_path, .. } => format!(
                "{} is a workweave index at the path earlier releases kept it, so every \
                     verb that reads the index refuses for this project; run \
                     `rwv doctor --fix` to convert it and move it into the project clone",
                index_path.display()
            ),
            CheckViolation::UnreadableOwnedState {
                project,
                state_path,
                error,
                active,
            } => {
                if active {
                    format!(
                        "{}: rwv's record of the generations it accepted does not \
                         parse ({error}); until it is rebuilt, the managed-file \
                         drift and derived-state staleness checks report nothing \
                         for project `{project}` — including for files that had \
                         already drifted. Run `rwv materialize` to rebuild it: it \
                         re-derives the generated files this project has and \
                         records them afresh, and leaves an empty record where it \
                         has none",
                        state_path.display()
                    )
                } else {
                    format!(
                        "{}: rwv's record of the generations it accepted does not \
                         parse ({error}); until it is rebuilt, the managed-file \
                         drift and derived-state staleness checks report nothing \
                         for project `{project}` — including for files that had \
                         already drifted. `{project}` is not the active project \
                         here, and `rwv materialize` takes no project argument — \
                         it acts only on the active one. Run `rwv activate \
                         {project}`; the repair runs there",
                        state_path.display()
                    )
                }
            }
            CheckViolation::UnreadableWorkweaveIndex {
                project,
                index_path,
                error,
            } => format!(
                "{}: workweave index does not parse ({error}); the recorded \
                     workweaves for project `{project}` are unevaluated. `rwv doctor \
                     --fix` rebuilds it from the markers of the workweaves in its \
                     containers; it cannot rebuild the basedir the index recorded, \
                     and a workweave outside those containers is listed again by the \
                     next rwv run inside it",
                index_path.display()
            ),
            // Relayed, not narrated: the loader states which file failed
            // and what to do about it, and this arm cannot know either.
            CheckViolation::UnparseableProject {
                project, message, ..
            } => format!("{project}: {message}"),
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
                        "working tree has live edits (once `rwv doctor --fix` has \
                             refreshed what beside them is stale, commit them if they are yours, \
                             or set them aside; not auto-fixed)"
                    }
                };
                format!("{location}: {detail}")
            }
            CheckViolation::DanglingActiveProject {
                project,
                missing_dir,
            } => format!(
                "active project `{}` is set in `.rwv-active` but `{}` does not exist; \
                     run `rwv activate <existing-project>` or remove `.rwv-active`",
                project,
                missing_dir.display()
            ),
            CheckViolation::UnreadableActiveProject { path, detail } => format!(
                "{} names no project rwv can select ({detail}); run \
                 `rwv activate <project>` to select one, or `rwv doctor --fix` to remove the file",
                path.display()
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
                        root.display()
                    ),
                    WeaveRootIdentityConflictKind::MarkerUnverifiable {
                        marker_path,
                        defect,
                    } => format!(
                        "{}: carries both `.rwv-active` (which {pointer}) and \
                             `.rwv-workweave`; the two are mutually exclusive. {} \
                             `--fix` does not touch the pointer until the marker is repaired",
                        root.display(),
                        defect.refusal(&marker_path.display())
                    ),
                    WeaveRootIdentityConflictKind::Unwitnessed { detail } => format!(
                        "{}: carries both `.rwv-active` (which {pointer}) and \
                             `.rwv-workweave`; the two are mutually exclusive. {detail} \
                             Nothing outside this directory says which file is the stray, so \
                             `--fix` does not touch either — delete the one you know to be \
                             wrong by hand",
                        root.display()
                    ),
                };
                msg
            }
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
                    WorkweaveTreeIntegrityKind::ForeignPrimaryOtherWorkspace { marker_primary } => {
                        format!(
                            "{}: belongs to workspace `{}`; not this workspace's to manage",
                            workweave_dir.display(),
                            marker_primary.display()
                        )
                    }
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
                             disk but not recorded in the workweave index; \
                             run `rwv doctor --fix` to adopt it",
                        workweave_dir.display(),
                        workweave_name,
                        project
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
                        workweave_dir.display(),
                    ),
                    WorkweaveTreeIntegrityKind::MisnamedDir {
                        expected_dir_name,
                        detail,
                    } => match expected_dir_name {
                        Some(expected) => format!(
                            "{}: workweave directory name disagrees with its records — \
                                 {detail}. Rename the directory to `{expected}` to restore \
                                 the recorded identity",
                            workweave_dir.display(),
                        ),
                        None => format!(
                            "{}: workweave directory name disagrees with its records — \
                                 {detail}. Rename it to the `<project>--<name>` you intend, \
                                 or remove the directory",
                            workweave_dir.display(),
                        ),
                    },
                };
                msg
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
                    format!(
                        "{project}: {repo}: origin URL mismatch — manifest has \
                             `{manifest_url}`, clone has `{actual_url}`; pushes may target \
                             the wrong remote (report-only; update the manifest or \
                             re-point the remote to converge){suffix}",
                    )
                }
                ProvenanceKind::RelativeRemote {
                    path,
                    remote,
                    manifest_remote,
                } => format!(
                    "{project}: {repo}: the clone records its `{remote}` remote as the \
                         relative path `{path}`, which git reads from each checkout's own \
                         directory, so the member's workweave checkouts reach other remotes \
                         than this one (report-only; record an absolute URL: `git -C {repo} \
                         remote set-url {remote} {url}`)",
                    url = manifest_remote.as_deref().unwrap_or("<url>"),
                ),
                ProvenanceKind::UnresolvableUrl { manifest_url } => format!(
                    "{project}: {repo}: url `{manifest_url}` is neither a URL nor registry \
                         shorthand, so it names no remote and no clone of the member can be \
                         made (report-only; write it in rwv.toml as a URL, `{file_url}` for a \
                         local remote)",
                    file_url = crate::path_spelling::file_url_suggestion(&manifest_url),
                ),
                ProvenanceKind::PlacementDisagreement {
                    expected_path,
                    converge: PlacementConvergence::FromPrimary,
                } => format!(
                    "{project}: {repo}: recorded at `{repo}`, but its URL places at \
                         `{expected_path}` under the canonical layout (report-only; converge \
                         it in the primary weave, where `rwv doctor` names the steps — moving \
                         the checkout here breaks it, since it is a worktree of the canonical \
                         clone; a `reference` or `fork` role exempts this comparison)",
                ),
                ProvenanceKind::PlacementDisagreement {
                    expected_path,
                    converge: PlacementConvergence::Here { retire_first },
                } => format!(
                    "{project}: {repo}: recorded at `{repo}`, but its URL places at \
                         `{expected_path}` under the canonical layout (report-only; to \
                         converge, {retire}move the checkout to `{expected_path}` unless it is \
                         already there, re-key the entry in rwv.toml from `{repo}` to \
                         `{expected_path}`, run `rwv lock`, and pin it with `rwv member pin \
                         {expected_path} <rev> --commit` — a `reference` or `fork` role \
                         exempts this comparison)",
                    retire = if retire_first.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "first land and retire each workweave holding a worktree of it, \
                             which the move would strand ({}): commit or set aside its edits and \
                             run `rwv land --retire` from it; then ",
                            retire_first
                                .iter()
                                .map(|w| format!("`{w}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    },
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
                             `.workweaves/` (`rwv doctor --fix` replaces it with a worktree of \
                             the canonical clone unless it holds commits, files or worktrees \
                             the canonical clone lacks)",
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
                msg
            }
            CheckViolation::Alignment {
                repo_path,
                sub_kind,
                branch,
                listed,
                ..
            } => {
                let on = branch
                    .as_deref()
                    .map(|b| format!(" `{b}`"))
                    .unwrap_or_default();
                let listing = if listed.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", listed.join("; "))
                };
                match sub_kind {
                    AlignmentKind::OnBranch => format!(
                        "{}: checkout is on branch{on}, not where its workspace expects it; \
                         `rwv update` moves it",
                        repo_path.display()
                    ),
                    AlignmentKind::OnBranchWithStrayCommits => format!(
                        "{}: checkout is on branch{on}, which is not where its workspace \
                         expects it and holds commits no other ref reaches{listing}; `rwv \
                         update` leaves it",
                        repo_path.display()
                    ),
                    AlignmentKind::Detached => format!(
                        "{}: checkout is detached where its workspace expects it elsewhere; \
                         `rwv update` returns it",
                        repo_path.display()
                    ),
                    AlignmentKind::DetachedWithUnpushedCommits => format!(
                        "{}: checkout is detached and holds commits no branch reaches\
                         {listing}; `rwv update` leaves it",
                        repo_path.display()
                    ),
                    AlignmentKind::Unaligned => format!(
                        "{}: the lock has no entry for this member and its expected branch \
                         exists nowhere",
                        repo_path.display()
                    ),
                    AlignmentKind::ProjectDetached => format!(
                        "{}: the project repo is on no branch{listing}",
                        repo_path.display()
                    ),
                }
            }
            CheckViolation::StaleWorktreeRegistration {
                workweave,
                repo,
                worktree_path,
                sub_kind,
            } => {
                let location = match workweave {
                    Some(ww) => format!("{ww}/{repo}"),
                    None => format!("{repo}"),
                };
                match sub_kind {
                    RegistrationDirectoryKind::DirectoryGone => format!(
                        "{location}: stale-worktree-registration: missing path \
                         {} (run `rwv doctor --fix` to prune)",
                        worktree_path.display()
                    ),
                    RegistrationDirectoryKind::DirectoryStands => format!(
                        "{location}: stale-worktree-registration: {} stands without \
                         the `.git` file that links it to the repo, so `rwv doctor \
                         --fix` keeps its registration. Move the directory away to \
                         keep its files, then run `rwv doctor --fix` to prune it",
                        worktree_path.display()
                    ),
                }
            }
            CheckViolation::StaleOpState {
                workspace_dir,
                verb,
                started_at,
            } => format!(
                "{}/.rwv-op: stale-op-state present (started_at={started_at}); \
                     resume with `{resume}`; or {abort}. \
                     Never auto-fixed — another terminal may be mid-conflict-resolution.",
                workspace_dir.display(),
                resume = crate::op_state::resume_command_at(verb, &workspace_dir),
                abort = crate::op_state::abort_rolls_back_at(&workspace_dir),
            ),
            CheckViolation::UnreadableOpRecord {
                workspace_dir,
                error,
            } => format!(
                "{}/.rwv-op: unreadable-op-record — {error}. Its savepoints are kept while \
                     it stands.",
                workspace_dir.display(),
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
                format!(
                    "{}/.rwv-op-lease: dead-op-lease op_id={op_id}{age_str} — {cause}; \
                         safe to auto-fix with `rwv doctor --fix` (removes the lease file).",
                    workspace_dir.display()
                )
            }
            CheckViolation::ClaimOwnerUnreachable {
                claim_dir,
                op_id,
                recorded_owner,
            } => format!(
                "{}: claim-owner-unreachable op_id={op_id} — the claim's owner workspace {} is \
                     not here to read, so whether its operation stands cannot be told; if it \
                     is gone, `rwv doctor --fix` releases the claim.",
                claim_dir.display(),
                recorded_owner.display()
            ),
            CheckViolation::ClaimUnreadable { file, seen } => format!(
                "{}: claim-unreadable — it holds what this rwv does not read ({}): an rwv of \
                 another release took it, or it is corrupt. No landing takes it over. End that \
                 landing with the release that started it; if none stands, `rwv doctor --fix \
                 --release-unreadable-claims` releases it, and a live landing of another \
                 release would lose its claim.",
                file.display(),
                unreadable_claim_text(&seen)
            ),
            CheckViolation::ClaimUnopenable { file, error } => format!(
                "{}: claim-unopenable — it cannot be read ({error}), so whose it is cannot be \
                 told and rwv releases nothing it cannot read; a landing into its target waits \
                 on it and refuses. If it is a file this user may not read, a landing run as \
                 another user holds it: end that landing as that user, or make the file \
                 readable to this one. If what stands there is not a file, rwv did not put it \
                 there: remove it.",
                file.display()
            ),
            CheckViolation::OrphanedSavepoint {
                workweave,
                repo,
                key,
                sub_kind,
            } => {
                let location = match workweave {
                    Some(ww) => format!("{ww}/{repo}"),
                    None => format!("{repo}"),
                };
                let detail = match sub_kind {
                    OrphanedSavepointKind::Redundant => {
                        "orphaned-savepoint (redundant — every tip it holds reached by a branch; \
                             run `rwv doctor --fix` to drop)"
                    }
                    OrphanedSavepointKind::Live => {
                        "orphaned-savepoint (last pointer to unreachable commits; \
                             run `rwv doctor --fix` to keep them as a backup)"
                    }
                };
                format!("{location}: {detail} op_id={key}")
            }
            CheckViolation::UnrecognizedSavepointRef {
                workweave,
                repo,
                holder,
                ref_name,
            } => {
                let location = match workweave {
                    Some(ww) => format!("{ww}/{repo}"),
                    None => format!("{repo}"),
                };
                let holder = holder.display().to_string();
                let holder = shell_word(&holder);
                format!(
                    "{location}: unrecognized-savepoint-ref {ref_name} — it sits in the \
                         savepoint namespace but no op id names it, so rwv did not file it. \
                         Never auto-fixed: what it points at is unknown. Inspect it, and \
                         delete it by hand with `git -C {holder} update-ref -d {ref_name}` \
                         once nothing needs it."
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
                msg
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
                format!(
                    "cargo patch shadowing: `[patch.{registry}].{crate_name}` in {} \
                         silently defeats the weave-level entry in {} \
                         (cargo merges .cargo/config.toml closest-wins per key, with no \
                         warning when a patch is inert)",
                    member_config.display(),
                    weave_config.display(),
                )
            }
            CheckViolation::MissingCanonicalClone {
                workweave,
                repo,
                canonical_path,
            } => {
                safe_to_fix = false;
                format!(
                    "{workweave}/{repo}: canonical clone for `{repo}` is absent \
                         (expected at {}), so git cannot open this worktree; run \
                         `rwv update --project {project}` at the primary weave to clone it \
                         again, then `rwv doctor --fix` there, which relinks this checkout \
                         keeping its files — where its pinned commits went with the deleted \
                         clone, the finding that follows names `--fix --abandon-lost-pins`",
                    canonical_path.display(),
                    project = workweave.project,
                )
            }
            CheckViolation::UninitializedSubmodule {
                workweave,
                repo,
                sub_kind,
                empty_paths,
            } => {
                let remedy = match sub_kind {
                    SubmoduleCommitKind::Fetched => {
                        "`rwv doctor --fix` checks them out from the canonical clone's \
                             copies"
                            .to_owned()
                    }
                    SubmoduleCommitKind::Unfetched => format!(
                        "their recorded commits are not in the canonical clone's copies; \
                             `rwv update --repo {repo}` run in that workweave fetches them \
                             there and checks them out"
                    ),
                };
                format!(
                    "{workweave}/{repo}: submodules not initialized — \
                         {n} submodule path(s) are empty on disk: {paths}; {remedy}",
                    n = empty_paths.len(),
                    paths = empty_paths.join(", "),
                )
            }
            CheckViolation::PhantomMergeDriver {
                repo,
                pattern,
                driver,
            } => {
                // No fix: a driver name is a guess at what the operator meant,
                // and deleting the line is a guess that they meant nothing.
                safe_to_fix = false;
                format!(
                    "{repo}: .gitattributes assigns merge driver `{driver}` to \
                         `{pattern}`, but rwv defines no driver by that name — the \
                         `rwv-` prefix is rwv's, so nothing else defines it either \
                         and the line silently does nothing (git falls back to a \
                         textual merge). Use `merge={defined}` for a derived path \
                         whose target-side copy should win, or drop the line",
                    defined = crate::git::DERIVED_CONTENT_DRIVER_NAME,
                )
            }
        };
        Some(Issue {
            kind: IssueKind::CoreFinding,
            source: IssueSource::Core,
            severity,
            message: message.into(),
            safe_to_fix,
        })
    }
}

// ---------------------------------------------------------------------------
// Drift: a checkout whose branch moved under it, read path by path
// ---------------------------------------------------------------------------

/// What may be done with one path of a checkout that differs from its HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathDrift {
    /// The branch moved under the checkout changed this path, and its index
    /// entry and file both still hold the moved-from tip's content: both take
    /// HEAD's, and nothing is lost that a commit does not hold.
    Stale,
    /// As [`PathDrift::Stale`], but the file already holds HEAD's content:
    /// only the index entry takes HEAD's.
    StaleRecord,
    /// The move changed this path and so did something else. Left as it
    /// stands.
    MovedAndEdited,
    /// A change to a path the move did not touch, or any change in a checkout
    /// no move under it is evidenced for: an uncommitted edit.
    Edited,
}

/// One checkout's differences from its HEAD, each path classified.
#[derive(Debug, Clone, Default)]
pub struct DriftReading {
    /// Where the checkout's branch moved from, when it moved under it.
    pub left: Option<CommitId>,
    /// Where the branch moved to, when it moved under the checkout.
    pub head: Option<CommitId>,
    pub paths: Vec<(PathStanding, PathDrift)>,
}

impl DriftReading {
    fn named(&self, drift: PathDrift) -> Vec<String> {
        self.paths
            .iter()
            .filter(|(_, d)| *d == drift)
            .map(|(p, _)| p.path.clone())
            .collect()
    }

    /// The paths whose index entry and file a refresh replaces.
    pub fn stale_files(&self) -> Vec<String> {
        self.named(PathDrift::Stale)
    }

    /// The paths whose index entry alone a refresh replaces.
    pub fn stale_records(&self) -> Vec<String> {
        self.named(PathDrift::StaleRecord)
    }

    /// The paths both the move and an edit here changed, of those the VCS
    /// compares with their recorded entries.
    pub fn moved_and_edited(&self) -> Vec<String> {
        self.paths
            .iter()
            .filter(|(_, d)| *d == PathDrift::MovedAndEdited)
            .map(|(p, _)| p.path.clone())
            .collect()
    }

    /// Of the paths both moved and edited, those where something stands in
    /// the way of what the moved tip puts there: a directory or an untracked
    /// file at its name, or a link or file in place of one of its leading
    /// directories.
    pub fn in_the_way(&self, repo: &Path) -> Vec<String> {
        self.paths
            .iter()
            .filter(|(p, d)| {
                *d == PathDrift::MovedAndEdited
                    && (!p.compared || behind_non_directory(repo, &p.path))
            })
            .map(|(p, _)| p.path.clone())
            .collect()
    }

    /// Where the checkout's branch moved, as a refusal's subject says it.
    pub fn moved(&self) -> String {
        format!(
            "its branch moved from {} to {} outside this checkout",
            self.left.as_ref().map(CommitId::short).unwrap_or_default(),
            self.head.as_ref().map(CommitId::short).unwrap_or_default()
        )
    }

    pub fn is_refreshable(&self) -> bool {
        self.paths
            .iter()
            .any(|(_, d)| matches!(d, PathDrift::Stale | PathDrift::StaleRecord))
    }

    /// The paths a refresh leaves differing from HEAD.
    pub fn kept(&self) -> impl Iterator<Item = &PathStanding> {
        self.paths
            .iter()
            .filter(|(_, d)| matches!(d, PathDrift::MovedAndEdited | PathDrift::Edited))
            .map(|(p, _)| p)
    }

    /// Whether a refresh replaces `path` in index or working tree.
    pub fn refreshes(&self, path: &str) -> bool {
        self.paths
            .iter()
            .any(|(p, d)| p.path == path && matches!(d, PathDrift::Stale | PathDrift::StaleRecord))
    }
}

/// Read `repo`'s checkout: where its branch moved from, when it moved under
/// it, and each differing path classified against that.
///
/// A path is drift only where both are shown: the branch moved under the
/// checkout, and the path still holds what the moved-from tip held. Without
/// that every difference is an edit, as it is in a checkout stopped in an
/// operation, and in one holding no record of where it stood. In a checkout
/// whose index already holds HEAD's entry for a path the move changed, a
/// checkout left behind holds the old tip at every such path it has not
/// edited, so one that took part of the move has had it taken by something
/// its history does not show: there every difference is an edit except a
/// path whose index entry is the old tip's and whose file is HEAD's, which is
/// all a refresh stopped between its two writes leaves.
pub fn read_drift(vcs: &dyn crate::vcs::Vcs, repo: &Path) -> Result<DriftReading, VcsError> {
    // Refreshing an index entry ends a merge, cherry-pick or revert in
    // progress, so a checkout stopped in one has no drift to refresh.
    let left = match vcs.mid_operation(repo) {
        Some(_) => None,
        None => vcs.left_behind_at(repo)?,
    };
    let standings = vcs.path_standings(repo, left.as_ref())?;
    if left.is_none()
        && vcs.mid_operation(repo).is_none()
        && standings
            .iter()
            .any(|standing| (standing.index, standing.file) != (Holds::Head, Holds::Head))
    {
        if let Some(why) = vcs.checkout_unrecorded(repo)? {
            return Err(VcsError::CheckoutUnrecorded {
                repo: repo.to_path_buf(),
                recording_off: (why == crate::vcs::Unrecorded::RecordingOff).then(|| {
                    format!(
                        "VCS action ({}: `{}`)",
                        vcs.name(),
                        vcs.hint(crate::vcs::VcsAction::TurnOnCheckoutRecording)
                            .replace("<checkout>", &repo.display().to_string())
                    )
                }),
            });
        }
    }
    let partial_take = standings
        .iter()
        .any(|standing| standing.moved && standing.index == Holds::Head);
    let head = match left {
        Some(_) => Some(vcs.head_revision(repo)?.id().clone()),
        None => None,
    };
    let paths = standings
        .into_iter()
        .filter(|standing| (standing.index, standing.file) != (Holds::Head, Holds::Head))
        .map(|standing| {
            let drift = match (
                left.is_some() && standing.moved,
                standing.index,
                standing.file,
            ) {
                (false, _, _) => PathDrift::Edited,
                (true, Holds::LeftTip, Holds::Head) => PathDrift::StaleRecord,
                (true, _, _) if partial_take => PathDrift::Edited,
                (true, Holds::LeftTip, Holds::LeftTip) => PathDrift::Stale,
                (true, _, _) => PathDrift::MovedAndEdited,
            };
            (standing, drift)
        })
        .collect();
    Ok(DriftReading { left, head, paths })
}

/// What [`refresh_drift`] replaced in one checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refreshed {
    pub moved_from: CommitId,
    pub moved_to: CommitId,
    pub files: Vec<String>,
    pub recorded_only: Vec<String>,
}

impl Refreshed {
    /// The report line, after the checkout's label.
    pub fn describe(&self) -> String {
        let mut said = format!(
            "refreshed drift: its branch moved from {} to {} outside this checkout",
            self.moved_from.short(),
            self.moved_to.short()
        );
        if !self.files.is_empty() {
            said.push_str(&format!(
                "; these files, and what the VCS records for them, held {}'s content and now hold {}'s: {}",
                self.moved_from.short(),
                self.moved_to.short(),
                self.files.join(", ")
            ));
        }
        if !self.recorded_only.is_empty() {
            said.push_str(&format!(
                "; what the VCS records for these paths was reset to {}'s, whose content the files already held: {}",
                self.moved_to.short(),
                self.recorded_only.join(", ")
            ));
        }
        said
    }

    /// Whether `path`'s index entry or file was replaced.
    pub fn replaced(&self, path: &str) -> bool {
        self.files
            .iter()
            .chain(&self.recorded_only)
            .any(|p| p == path)
    }
}

/// Why [`refresh_drift`] left stale paths as they stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeftWhy {
    /// A path no longer held what it was read holding, or something on disk
    /// stood in the way of the write.
    Changed,
    /// The branch moved again after the checkout was read.
    MovedAgain,
    /// The index holds unresolved conflicts.
    Unresolved,
    /// The VCS could not write; its own account.
    Failed(String),
}

impl LeftWhy {
    fn describe(&self) -> String {
        match self {
            Self::Changed => "a path changed after it was read, or something on disk is \
                              in the way of the write"
                .to_owned(),
            Self::MovedAgain => "the branch moved again after the checkout was read".to_owned(),
            Self::Unresolved => "the checkout holds unresolved conflicts".to_owned(),
            Self::Failed(said) => format!(
                "the write failed: {}",
                said.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
            ),
        }
    }
}

/// The stale paths a refresh did not write, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftInPlace {
    pub paths: Vec<String>,
    pub why: LeftWhy,
}

impl LeftInPlace {
    /// What the paths still hold, and why: the subject of the line naming them.
    pub fn describe(&self) -> String {
        self.why.describe()
    }
}

/// What [`refresh_drift`] did in one checkout: what it replaced, the stale
/// paths it left as they stand, and a record of the refresh that could not be
/// written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriftRefresh {
    pub refreshed: Option<Refreshed>,
    pub left_in_place: Option<LeftInPlace>,
    pub unrecorded: Option<String>,
    /// Files both moved and edited, for which nothing in the checkout was
    /// written.
    pub held: Vec<String>,
}

impl DriftRefresh {
    /// The line naming what was left in place in the checkout at `dir`, after
    /// its label, with the route that clears it: `again`, the command that
    /// reads the checkout afresh, once nothing else is changing it.
    pub fn describe_left_in_place(
        &self,
        vcs: &dyn crate::vcs::Vcs,
        dir: &Path,
        again: &str,
    ) -> Option<String> {
        let left = self.left_in_place.as_ref()?;
        let route = match left.why {
            LeftWhy::Unresolved => format!(
                "resolve the conflicted paths `{}` lists, then run `{again}`",
                vcs.hint(crate::vcs::VcsAction::Status)
                    .replace("<checkout>", &dir.display().to_string())
            ),
            _ => format!("run `{again}`, which reads them afresh"),
        };
        Some(format!(
            "drift left in place ({}): these paths still hold the old tip's content, and \
             committing them would record the move's reverse; {route}: {}",
            left.describe(),
            left.paths.join(", ")
        ))
    }

    /// The line naming the files both moved and edited that held the whole
    /// checkout back, after its label, with the choices that clear it.
    pub fn describe_held(&self) -> Option<String> {
        if self.held.is_empty() {
            return None;
        }
        Some(format!(
            "drift held: these files were changed both by the branch's move and by an edit \
             here, so nothing in this checkout is refreshed; merge each edit onto the moved \
             tip with `rwv doctor --fix --merge-edited-drift`, or set the edits aside in a \
             backup with `rwv doctor --fix --set-aside-edited-drift`: {}",
            self.held.join(", ")
        ))
    }

    /// The line saying the refresh went unrecorded, after the checkout's label.
    pub fn describe_unrecorded(&self) -> Option<String> {
        self.unrecorded.as_ref().map(|said| {
            format!("the refresh was written but could not be recorded in the checkout: {said}")
        })
    }
}

/// Replace what `reading` found stale, writing only what a second reading,
/// taken immediately before, agrees on, each write made only where every path
/// in it still holds the moved-from tip's content; and, once nothing stale is
/// left, record that the checkout stands at its HEAD. The files are written
/// first and every index entry after them in one write, so a run stopped or
/// declined between the two leaves each path's index entry at the moved-from
/// tip, absent where that tip lacks the path, beside what HEAD puts on disk
/// there, which reads as drift again.
pub fn refresh_drift(
    vcs: &dyn crate::vcs::Vcs,
    repo: &Path,
    reading: &DriftReading,
) -> Result<DriftRefresh, VcsError> {
    let (Some(moved_from), Some(moved_to)) = (&reading.left, &reading.head) else {
        return Ok(DriftRefresh::default());
    };
    let held = reading.moved_and_edited();
    if !held.is_empty() {
        return Ok(DriftRefresh {
            held,
            ..DriftRefresh::default()
        });
    }
    if !reading.is_refreshable() {
        return Ok(DriftRefresh::default());
    }
    let left_in_place = |paths: BTreeSet<String>, why: LeftWhy| DriftRefresh {
        left_in_place: Some(LeftInPlace {
            paths: paths.into_iter().collect(),
            why,
        }),
        ..DriftRefresh::default()
    };
    let read_stale = |r: &DriftReading| -> BTreeSet<String> {
        r.stale_files()
            .into_iter()
            .chain(r.stale_records())
            .collect()
    };
    let first = read_stale(reading);
    crate::test_pause::point("drift-read");
    let now = read_drift(vcs, repo)?;
    if (&now.left, &now.head) != (&reading.left, &reading.head) {
        let why = match vcs.head_revision(repo)?.id() == moved_to {
            true => LeftWhy::Changed,
            false => LeftWhy::MovedAgain,
        };
        return Ok(left_in_place(
            first.union(&read_stale(&now)).cloned().collect(),
            why,
        ));
    }
    let agreed = |of: fn(&DriftReading) -> Vec<String>| -> Vec<String> {
        let again: BTreeSet<String> = of(&now).into_iter().collect();
        of(reading)
            .into_iter()
            .filter(|p| again.contains(p))
            .collect()
    };
    let files = agreed(DriftReading::stale_files);
    let recorded_only = agreed(DriftReading::stale_records);
    let disagreed: BTreeSet<String> = first
        .union(&read_stale(&now))
        .filter(|p| !files.contains(p) && !recorded_only.contains(p))
        .cloned()
        .collect();
    if !disagreed.is_empty() {
        return Ok(left_in_place(
            first.union(&disagreed).cloned().collect(),
            LeftWhy::Changed,
        ));
    }
    crate::test_pause::point("drift-write");
    if vcs.head_revision(repo)?.id() != moved_to {
        return Ok(left_in_place(first, LeftWhy::MovedAgain));
    }
    let declined = |write: RefreshWrite, paths: &[String]| -> Result<Option<LeftWhy>, VcsError> {
        Ok(match write {
            RefreshWrite::Written => None,
            RefreshWrite::Unresolved => Some(LeftWhy::Unresolved),
            RefreshWrite::Declined { vcs_said } => {
                let after = read_drift(vcs, repo)?;
                Some(if vcs.head_revision(repo)?.id() != moved_to {
                    LeftWhy::MovedAgain
                } else if after.left == reading.left && paths.iter().all(|p| after.refreshes(p)) {
                    LeftWhy::Failed(vcs_said)
                } else {
                    LeftWhy::Changed
                })
            }
        })
    };
    let written =
        vcs.refresh_paths_if_left(repo, moved_from, moved_to, &files, RefreshScope::FilesOnly)?;
    if let Some(why) = declined(written, &files)? {
        return Ok(left_in_place(first, why));
    }
    crate::test_pause::point("drift-write-records");
    let entries: Vec<String> = files.iter().chain(&recorded_only).cloned().collect();
    let written = vcs.refresh_paths_if_left(
        repo,
        moved_from,
        moved_to,
        &entries,
        RefreshScope::RecordOnly,
    )?;
    if let Some(why) = declined(written, &entries)? {
        return Ok(left_in_place(first, why));
    }
    Ok(DriftRefresh {
        refreshed: Some(Refreshed {
            moved_from: moved_from.clone(),
            moved_to: moved_to.clone(),
            files,
            recorded_only,
        }),
        left_in_place: None,
        unrecorded: vcs
            .record_checkout_at_head(repo, moved_to)
            .err()
            .map(|e| format!("{e:#}")),
        held: Vec::new(),
    })
}

/// Whether something other than a directory stands in place of one of
/// `path`'s leading directories under `repo`.
fn behind_non_directory(repo: &Path, path: &str) -> bool {
    let mut at = repo.to_path_buf();
    let components: Vec<&str> = path.split('/').collect();
    for component in &components[..components.len().saturating_sub(1)] {
        at.push(component);
        match std::fs::symlink_metadata(&at) {
            Err(_) => return false,
            Ok(meta) if !meta.is_dir() => return true,
            Ok(_) => {}
        }
    }
    false
}

/// What [`merge_edited_drift`] or [`carry_set_back`] did in one checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditedDriftMerge {
    /// No move of the checkout's branch under it is shown, so there is no
    /// tip it was at to merge from.
    NoMove,
    /// The checkout holds no file both moved and edited.
    NothingHeld,
    /// The checkout was set back and the VCS refused the next step, in its
    /// own words. A refusal git makes before its write leaves the files as
    /// they stood; one during the write can leave some of them moved, and
    /// the backup taken first holds the edits either way.
    Refused {
        back: crate::vcs::SetBack,
        said: String,
        backup: Option<crate::diagnosis::KeptBackup>,
    },
    Carried {
        backup: Option<String>,
        backup_tip: Option<String>,
        moved_from: CommitId,
        moved_to: CommitId,
        clean: Vec<String>,
        took_incoming: Vec<String>,
        conflicted: Vec<String>,
        set_aside: Option<CommitId>,
        /// What an earlier run's set-aside kept, oldest first.
        earlier: Vec<CommitId>,
    },
}

/// The words naming `backup`, or that none was needed.
pub fn backup_named(backup: Option<&str>) -> String {
    match backup {
        Some(backup) => format!("backup {backup} (`rwv backup list`)"),
        None => "no backup, the checkout holding no edit to keep".to_owned(),
    }
}

impl EditedDriftMerge {
    /// The report line of a carried merge or set-aside, after the checkout's
    /// label.
    pub fn describe_carried(&self, vcs: &dyn crate::vcs::Vcs, dir: &Path) -> Option<String> {
        let Self::Carried {
            backup,
            backup_tip,
            moved_from,
            moved_to,
            clean,
            took_incoming,
            conflicted,
            set_aside,
            earlier,
        } = self
        else {
            return None;
        };
        let spelled = |action, file: &str| {
            vcs.hint(action)
                .replace("<checkout>", &dir.display().to_string())
                .replace("<tip>", backup_tip.as_deref().unwrap_or_default())
                .replace("<file>", file)
        };
        let take_back = |stash: &CommitId| {
            spelled(crate::vcs::VcsAction::TakeBackSetAside, "")
                .replace("<set-aside>", stash.as_str())
        };
        let mut said = String::new();
        if let Some(backup) = backup {
            said.push_str(&format!(
                "its working copy and the VCS's record of it are kept as backup {backup} (`rwv backup list`); "
            ));
        }
        said.push_str(&format!(
            "its branch moved from {} to {} outside this checkout, and the checkout now stands \
             at {}",
            moved_from.short(),
            moved_to.short(),
            moved_to.short()
        ));
        if !clean.is_empty() {
            said.push_str(&format!(
                "; these edits were merged onto {}'s content and stand as edits: {}",
                moved_to.short(),
                clean.join(", ")
            ));
        }
        let mut shown = Vec::new();
        if !earlier.is_empty() {
            said.push_str(&format!(
                "; the edits an earlier set-aside kept are in {}",
                earlier
                    .iter()
                    .map(CommitId::short)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(stash) = set_aside {
            said.push_str(&format!(
                "; these files' edits were set aside as {}, so each holds {}'s content: {}",
                stash.short(),
                moved_to.short(),
                took_incoming.join(", ")
            ));
        }
        let heading = match earlier.is_empty() {
            true => "\nthe set-aside edits merge back onto the checkout with:",
            false => {
                "\nthe set-aside edits merge back onto the checkout with, oldest first; record or \
                 commit what each brings back before the next:"
            }
        };
        for stash in earlier.iter().chain(set_aside) {
            shown.push((heading, take_back(stash)));
        }
        if set_aside.is_none() && !took_incoming.is_empty() {
            said.push_str(&format!(
                "; these files now hold {}'s content, a declared derived file's or one whose \
                 edit the move already made, and the version edited here is kept only in {}: {}",
                moved_to.short(),
                backup_named(backup.as_deref()),
                took_incoming.join(", ")
            ));
            if backup_tip.is_some() {
                for file in took_incoming {
                    shown.push((
                        "\nthe version each held here is shown by:",
                        spelled(crate::vcs::VcsAction::ShowFromBackup, file),
                    ));
                }
            }
        }
        if !conflicted.is_empty() {
            said.push_str(&format!(
                "; these did not merge and are left conflicted, holding both versions: \
                 resolve each, then `{}`: {}",
                vcs.hint(crate::vcs::VcsAction::MarkResolved)
                    .replace("<checkout>", &dir.display().to_string())
                    .replace("<paths>", &conflicted.join(" ")),
                conflicted.join(", ")
            ));
        }
        let mut headed = None;
        for (heading, line) in shown {
            if headed != Some(heading) {
                said.push_str(heading);
                headed = Some(heading);
            }
            said.push_str(&format!("\n    {line}"));
        }
        Some(said)
    }
}

/// Carry each edit in a checkout whose branch moved under it onto the tip
/// it moved to: with `Merge`, the working copy and index are kept as a
/// backup first, and the VCS merges the edits; with `SetAside`, the VCS sets
/// the edits to the files `reading` found both moved and edited aside. Both
/// first point the checkout at the tip its files hold, so the VCS sees the
/// edits alone; where it then refuses, the checkout stays there, set back.
pub fn merge_edited_drift(
    vcs: &dyn crate::vcs::Vcs,
    repo: &Path,
    reading: &DriftReading,
    resolution: crate::vcs::EditResolution,
) -> Result<EditedDriftMerge, VcsError> {
    let Some(moved_from) = &reading.left else {
        return Ok(EditedDriftMerge::NoMove);
    };
    let held = reading.moved_and_edited();
    if held.is_empty() {
        return Ok(EditedDriftMerge::NothingHeld);
    }
    crate::test_pause::point("drift-merge-read");
    let kept = match resolution {
        crate::vcs::EditResolution::Merge => vcs.backup_edits(repo)?,
        crate::vcs::EditResolution::SetAside => None,
    };
    vcs.set_back(repo, moved_from)?;
    crate::test_pause::point("drift-merge-set-back");
    let back = vcs
        .set_back_from(repo)?
        .ok_or_else(|| VcsError::CommandFailed {
            args: vec!["set back".to_owned()],
            repo: repo.to_path_buf(),
            stderr: "HEAD did not stay where it was pointed".to_owned(),
        })?;
    carry(vcs, repo, &back, resolution, &held, kept)
}

/// [`merge_edited_drift`] resumed in a checkout it left set back: every
/// edit standing there is carried onto wherever the branch is now.
pub fn carry_set_back(
    vcs: &dyn crate::vcs::Vcs,
    repo: &Path,
    back: &crate::vcs::SetBack,
    resolution: crate::vcs::EditResolution,
) -> Result<EditedDriftMerge, VcsError> {
    let mut edits = vcs.staged_against_head(repo)?;
    for path in vcs.unstaged_paths(repo)? {
        if !edits.contains(&path) {
            edits.push(path);
        }
    }
    let kept = match resolution {
        crate::vcs::EditResolution::Merge => vcs.backup_edits(repo)?,
        crate::vcs::EditResolution::SetAside => None,
    };
    let mut carried = carry(vcs, repo, back, resolution, &edits, kept)?;
    if let EditedDriftMerge::Carried {
        set_aside, earlier, ..
    } = &mut carried
    {
        *earlier = vcs.set_aside_from(repo, &back.at, &back.branch)?;
        earlier.retain(|stash| Some(stash) != set_aside.as_ref());
    }
    Ok(carried)
}

fn carry(
    vcs: &dyn crate::vcs::Vcs,
    repo: &Path,
    back: &crate::vcs::SetBack,
    resolution: crate::vcs::EditResolution,
    held: &[String],
    kept: Option<crate::vcs::BackupWitness>,
) -> Result<EditedDriftMerge, VcsError> {
    let backup = kept.as_ref().map(|kept| kept.backup().name.to_string());
    let backup_tip = kept.map(|kept| kept.backup().tip.id().as_str().to_owned());
    let refused = |said: String| EditedDriftMerge::Refused {
        back: back.clone(),
        said,
        backup: backup
            .clone()
            .zip(backup_tip.clone())
            .map(|(name, tip)| crate::diagnosis::KeptBackup { name, tip }),
    };
    let mut set_aside = None;
    if resolution == crate::vcs::EditResolution::SetAside && !held.is_empty() {
        match vcs.set_edits_aside(repo, held, &back.branch)? {
            crate::vcs::Refusable::Done(stash) => {
                eprintln!(
                    "rwv: {}: the edits are set aside as {}; they merge back onto the checkout \
                     with:\n    {}",
                    repo.display(),
                    stash.short(),
                    vcs.hint(crate::vcs::VcsAction::TakeBackSetAside)
                        .replace("<checkout>", &repo.display().to_string())
                        .replace("<set-aside>", stash.as_str())
                );
                set_aside = Some(stash)
            }
            crate::vcs::Refusable::Refused(said) => return Ok(refused(said)),
        }
    }
    crate::test_pause::point("drift-merge-carry");
    let checked_out = match resolution {
        crate::vcs::EditResolution::Merge => vcs.carry_edits_onto(repo, &back.branch)?,
        crate::vcs::EditResolution::SetAside => vcs.check_out_branch(repo, &back.branch)?,
    };
    if let crate::vcs::Refusable::Refused(said) = checked_out {
        return Ok(refused(said));
    }
    let conflicted = vcs.unmerged_paths(repo)?;
    let mut standing = vcs.staged_paths(repo)?;
    standing.extend(vcs.unstaged_paths(repo)?);
    let (clean, took_incoming): (Vec<String>, Vec<String>) = held
        .iter()
        .filter(|path| !conflicted.contains(path))
        .cloned()
        .partition(|path| set_aside.is_none() && standing.contains(path));
    Ok(EditedDriftMerge::Carried {
        backup,
        backup_tip,
        moved_from: back.at.clone(),
        moved_to: vcs.head_revision(repo)?.id().clone(),
        clean,
        took_incoming,
        conflicted,
        set_aside,
        earlier: Vec::new(),
    })
}

/// The `record-drift` and `working-tree-drift` findings one checkout's reading
/// yields: safe-to-fix for what a refresh replaces, live for what it keeps.
pub fn drift_findings(reading: &DriftReading) -> (Vec<RecordDriftKind>, Vec<WorkingTreeDriftKind>) {
    let mut index = Vec::new();
    let mut working_tree = Vec::new();
    if reading.is_refreshable() {
        index.push(RecordDriftKind::SafeToFix);
    }
    if !reading.stale_files().is_empty() {
        working_tree.push(WorkingTreeDriftKind::SafeToFix);
    }
    if reading.kept().any(|p| p.index != Holds::Head) {
        index.push(RecordDriftKind::LiveRecorded);
    }
    if reading.kept().any(|p| p.file != Holds::Head) {
        working_tree.push(WorkingTreeDriftKind::LiveEdits);
    }
    (index, working_tree)
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
/// The reported path names the repo directory rather than its store, so it
/// matches DanglingReference's repair target.
pub fn worktree_canonical_clone_missing(repo: &Path) -> Option<PathBuf> {
    let canonical = crate::git::linked_clone_dir(repo)?;
    if canonical.exists() {
        None
    } else {
        Some(canonical.to_path_buf())
    }
}

/// Execute `rwv doctor --locked` for the current workspace context.
///
/// Compares each repo's HEAD SHA against its `rwv.lock` entry. Outputs per-repo
/// status to stdout. Returns `Ok(true)` if any repo's tip differs from its lock
/// entry (exit 1), `Ok(false)` if all match (exit 0).
///
/// Total over the **raw** lock, which is what separates it from the pipeline's
/// [`CheckViolation::Pin`] rather than making it a second spelling of it:
/// an entry whose repo is absent from disk gets a verdict here and is
/// unreachable there, because [`crate::manifest::LockFile::resolve_versions`]
/// drops it and [`find_violations`] has no disk to re-read. Where both surfaces
/// do see an entry they must name the same two revisions in the same spelling —
/// pinned in `tests/doctor/lock_totality_agreement_test.rs`, which is also the only
/// coverage of the divergence.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_check_locked(ctx: &crate::workspace::WorkspaceContext) -> anyhow::Result<Verdict> {
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
        let project_dir = project_dir(&workspace_dir, pname);
        let project = match Project::from_dir(&project_dir, pname.clone()) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{}", crate::manifest::unreadable_project_warning(pname, &e));
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

            if !raw_entry.version.is_commit_id() {
                outln!(
                    "warning: {}",
                    crate::lock::pinned_by_name_warning(repo_path, &raw_entry.version)
                );
            }

            let actual = match vcs.head_revision(&repo_abs) {
                Ok(rev) => rev,
                Err(_) => {
                    outln!(
                        "{repo_path}: missing on disk (lock pins {}); run `rwv update` to \
                         materialize it",
                        raw_entry.version.short_display()
                    );
                    any_drift = true;
                    continue;
                }
            };

            if let Some(raw_rev) = unresolved.get(repo_path) {
                outln!(
                    "{repo_path}: lock pins unknown revision {}",
                    raw_rev.short_display()
                );
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
                outln!("{repo_path}: ok");
            } else {
                outln!(
                    "{repo_path}: tip {} ≠ lock {}",
                    actual.short_display(),
                    resolved_entry.version.short_display()
                );
                any_drift = true;
            }
        }
    }

    Ok(if any_drift {
        Verdict::Fail
    } else {
        Verdict::Pass
    })
}

/// Repairs that have to land before the workspace is read. A legacy marker
/// leaves a workweave unusable, so migrating it is what lets the rest of the
/// run resolve context at all.
///
/// The `[fixed]` announcement each arm prints is a checked surface, not
/// chatter: tests/doctor/doctor_fix_authority_test.rs binds every announcement in
/// this file to a finding [`CheckViolation::fix_disposition`] marks Auto or
/// Consented. An arm added here for a report-only finding fails there
/// instead of running unsanctioned.
fn apply_prelude_repairs(ctx: &crate::workspace::WorkspaceContext, fix_errors: &mut Vec<String>) {
    for finding in scan_for_legacy_workweave_markers(ctx.primary_path()) {
        match fix_legacy_workweave_marker(&finding) {
            Ok(true) => outln!(
                "[fixed] core: migrated legacy workweave marker {} to JSON",
                finding.marker_path.display()
            ),
            Ok(false) => {}
            Err(e) => fix_errors.push(format!("legacy workweave marker fix failed: {e:#}")),
        }
    }
}

/// `--fix` arms that repair the workspace rather than a collected finding.
///
/// They run before [`collect_doctor_violations`], which is what lets a
/// repaired workspace report as healthy instead of surfacing both `[fixed]`
/// and the warning it just resolved.
///
/// Every arm reads state the workspace holds in exactly one place — the
/// workweave index lives only in primary — so they take `primary_path()` and
/// ignore which weave invoked doctor. Arms repairing per-weave state must take
/// `active_path()` instead.
///
/// This pass runs before collection and names no [`CheckViolation`], so the
/// runtime disposition gate in [`apply_finding_repairs`] never sees it. What
/// holds it to the register instead is the `[fixed]` announcement each arm
/// prints: tests/doctor/doctor_fix_authority_test.rs binds every announcement to a
/// finding the register marks Auto or Consented, so an arm added here for a
/// report-only finding is a red test rather than dead code.
fn apply_workspace_repairs(
    ctx: &crate::workspace::WorkspaceContext,
    world: &DoctorWorld,
    fix_errors: &mut Vec<String>,
) {
    let vcs = world.vcs.as_ref();
    let project_scope = world.project_scope();

    match primary_pointer(ctx.primary_path()) {
        Some(PointerContent::Selects(active_name))
            if !project_dir(ctx.primary_path(), &active_name).is_dir() =>
        {
            match crate::workspace::clear_active_project(ctx.primary_path()) {
                Ok(()) => outln!(
                    "[fixed] core: cleared `.rwv-active` (was pointing at missing project `{}`)",
                    active_name
                ),
                Err(e) => fix_errors.push(format!(
                    "dangling-active-project fix failed for `{}`: {e:#}",
                    active_name
                )),
            }
        }
        Some(PointerContent::Unreadable { path, .. }) => {
            match crate::workspace::clear_active_project(ctx.primary_path()) {
                Ok(()) => outln!(
                    "[fixed] core: removed {} (it named no project rwv can select)",
                    path
                ),
                Err(e) => fix_errors.push(format!(
                    "unreadable-active-project fix failed for {}: {e:#}",
                    path
                )),
            }
        }
        Some(PointerContent::Selects(_)) | None => {}
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
                Ok(()) => outln!(
                    "[fixed] core: deleted `.rwv-active` at {} \
                     (redundant with the `.rwv-workweave` marker of registered workweave \
                     `{workweave_name}` in project `{project}`; the marker is unchanged)",
                    root.display()
                ),
                Err(e) => fix_errors.push(format!(
                    "weave-root-identity-conflict fix failed for {}: {e:#}",
                    root.display()
                )),
            }
        }
    }

    for project in crate::workspace::discover_projects(ctx.primary_path()) {
        if let Some(active) = project_scope {
            if &project != active {
                continue;
            }
        }
        match crate::workweave_index::convert_legacy_index(ctx.primary_path(), &project) {
            Ok(Some(converted)) => outln!(
                "[fixed] core: converted {} and moved it to {} ({} workweave(s) kept)",
                converted.from,
                converted.to,
                converted.workweaves
            ),
            Ok(None) => {}
            Err(e) => fix_errors.push(format!(
                "failed to convert {}'s workweave index: {e:#}",
                project
            )),
        }
    }

    for project in &world.input.projects {
        let project_repo = project_dir(&world.workspace_dir, &project.name);
        if !project_repo.is_dir() || vcs.lock_driver_setup(&project_repo).is_err() {
            continue;
        }
        match crate::lock_driver::migrate(vcs, &project_repo) {
            Ok(migrated) => {
                if migrated.committed {
                    outln!(
                        "[fixed] core: set up the lock merge driver's attribute line in {}, \
                         committing only that",
                        project.name
                    );
                } else if migrated.declared {
                    outln!(
                        "[fixed] core: wrote the lock merge driver's attribute line in {}",
                        project.name
                    );
                }
                if let Some(why) = migrated.withheld {
                    fix_errors.push(format!(
                        "{}: the lock merge driver's attribute line was not migrated: {why}",
                        project.name
                    ));
                }
                if migrated.defined {
                    outln!(
                        "[fixed] core: defined the lock merge driver in {}'s config",
                        project.name
                    );
                }
            }
            Err(e) => fix_errors.push(format!(
                "{}: failed to set up the lock merge driver: {e:#}",
                project.name
            )),
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
/// What an unreadable claim holds, as a report prints it: trimmed, invalid
/// UTF-8 replaced, and cut short where it runs long.
fn unreadable_claim_text(seen: &[u8]) -> String {
    const SHOWN: usize = 120;
    let text = String::from_utf8_lossy(seen);
    let text = text.trim();
    match text.char_indices().nth(SHOWN) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None if text.is_empty() => "nothing".to_owned(),
        None => text.to_owned(),
    }
}

/// Files both moved and edited, as `--fix` meets them: whether it may merge
/// them, the checkouts it wrote nothing to, and whether any held such a file.
struct HeldDrift {
    resolving: Option<crate::vcs::EditResolution>,
    refused: crate::diagnosis::Report,
    merge_found: bool,
    unread: bool,
    /// Whether this run refreshed a checkout's drift, which is where a held
    /// checkout whose in-the-way paths were moved aside ends up.
    refreshed: bool,
    homes: Homes,
}

/// Where the command runs, and the workspace each scanned checkout is in.
#[derive(Default)]
struct Homes {
    here: std::path::PathBuf,
    primary: std::path::PathBuf,
    workweaves: std::collections::HashMap<WorkweaveId, std::path::PathBuf>,
}

impl Homes {
    /// The workspace a checkout keyed by `workweave` is in, where that is
    /// not the one the command runs in.
    fn elsewhere(&self, workweave: &Option<WorkweaveId>) -> Option<&std::path::Path> {
        let home = match workweave {
            Some(ww) => self.workweaves.get(ww)?,
            None => &self.primary,
        };
        let same = crate::path_spelling::CanonicalPath::of(home)
            == crate::path_spelling::CanonicalPath::of(&self.here);
        (!same).then_some(home.as_path())
    }
}

/// The checkouts `--fix` writes nothing to for their edits' sake, judged
/// into `held.refused`: a checkout left set back, and files both moved and
/// edited, which are carried on where `held.resolving` consents and the
/// checkout is in the workspace the command runs in, and judged otherwise;
/// `merged` holds the checkouts carried on.
struct HeldPass {
    held: std::collections::HashSet<std::path::PathBuf>,
    merged: std::collections::HashSet<std::path::PathBuf>,
}

fn settle_held_drift(
    vcs: &dyn crate::vcs::Vcs,
    violations: &[CheckViolation],
    repo_locations: &std::collections::HashMap<(Option<WorkweaveId>, RepoPath), std::path::PathBuf>,
    held: &mut HeldDrift,
    fix_errors: &mut Vec<String>,
) -> HeldPass {
    let location_of = |workweave: &Option<WorkweaveId>, repo: &RepoPath| match workweave {
        Some(ww) => format!("{ww}/{repo}"),
        None => format!("{repo}"),
    };
    let mut held_checkouts = std::collections::HashSet::new();
    let mut merged_checkouts = std::collections::HashSet::new();
    let mut drifted: Vec<(String, Option<WorkweaveId>, std::path::PathBuf)> = Vec::new();
    for v in violations {
        if let CheckViolation::RecordDrift {
            workweave, repo, ..
        }
        | CheckViolation::WorkingTreeDrift {
            workweave, repo, ..
        } = v
        {
            if let Some(repo_abs) = repo_locations.get(&(workweave.clone(), repo.clone())) {
                if !drifted.iter().any(|(_, _, at)| at == repo_abs) {
                    drifted.push((
                        location_of(workweave, repo),
                        workweave.clone(),
                        repo_abs.clone(),
                    ));
                }
            }
        }
    }
    let mut merged_any = false;
    let mut unread = false;
    let mut locations: Vec<(String, Option<WorkweaveId>, std::path::PathBuf)> = repo_locations
        .iter()
        .map(|((workweave, repo), at)| {
            (location_of(workweave, repo), workweave.clone(), at.clone())
        })
        .collect();
    locations.sort();
    let mut carried = |location: &str,
                       repo_abs: &std::path::Path,
                       outcome: Result<EditedDriftMerge, VcsError>,
                       refused: &mut crate::diagnosis::Report,
                       fix_errors: &mut Vec<String>|
     -> bool {
        match outcome {
            Ok(EditedDriftMerge::NoMove | EditedDriftMerge::NothingHeld) => true,
            Ok(EditedDriftMerge::Refused { back, said, backup }) => {
                if let Some(kept) = &backup {
                    let staged = vcs
                        .staged_versions_against_head(repo_abs)
                        .unwrap_or_default();
                    if !staged.is_empty() {
                        outln!(
                            "rwv doctor: {location}: each recorded version is kept in {}, shown by:",
                            backup_named(Some(&kept.name))
                        );
                        for file in &staged {
                            outln!(
                                "    {}",
                                vcs.hint(crate::vcs::VcsAction::ShowStagedFromBackup)
                                    .replace("<checkout>", &repo_abs.display().to_string())
                                    .replace("<tip>", &kept.tip)
                                    .replace("<file>", file)
                            );
                        }
                    }
                }
                refused.add(
                    crate::diagnosis::set_back(
                        vcs,
                        location,
                        repo_abs,
                        &back,
                        Some(&said),
                        backup.as_ref(),
                    )
                    .checkout(repo_abs)
                    .judge(vcs),
                );
                false
            }
            Ok(outcome) => {
                if let Some(said) = outcome.describe_carried(vcs, repo_abs) {
                    outln!("[fixed] core: drift in {location}: {said}");
                }
                merged_checkouts.insert(repo_abs.to_path_buf());
                true
            }
            Err(e) => {
                fix_errors.push(format!("{location}: merging edited drift failed: {e:#}"));
                false
            }
        }
    };
    for (location, workweave, repo_abs) in &locations {
        if !vcs.is_repo(repo_abs) {
            continue;
        }
        let back = match vcs.set_back_from(repo_abs) {
            Ok(Some(back)) => back,
            Ok(None) => continue,
            Err(e) => {
                fix_errors.push(format!("{location}: its HEAD could not be read: {e:#}"));
                held_checkouts.insert(repo_abs.clone());
                continue;
            }
        };
        held_checkouts.insert(repo_abs.clone());
        let elsewhere = held
            .homes
            .elsewhere(workweave)
            .map(std::path::Path::to_path_buf);
        let judged = || {
            let judging = crate::diagnosis::set_back(vcs, location, repo_abs, &back, None, None)
                .checkout(repo_abs);
            match &elsewhere {
                Some(home) => judging.elsewhere(home),
                None => judging,
            }
        };
        merged_any |= held.resolving.is_some();
        let mut edits = vcs.staged_paths(repo_abs).unwrap_or_default();
        edits.extend(vcs.unstaged_paths(repo_abs).unwrap_or_default());
        let carries = !crate::diagnosis::carried_over(vcs, repo_abs, &back, &edits)
            .is_ok_and(|uncompared| uncompared.is_empty());
        let Some(resolution) = held.resolving.filter(|_| elsewhere.is_none() && !carries) else {
            held.refused.add(judged().judge(vcs));
            continue;
        };
        let outcome = carry_set_back(vcs, repo_abs, &back, resolution);
        if carried(location, repo_abs, outcome, &mut held.refused, fix_errors) {
            held_checkouts.remove(repo_abs);
        }
    }
    for (location, workweave, repo_abs) in &drifted {
        if held_checkouts.contains(repo_abs) {
            continue;
        }
        let reading = match read_drift(vcs, repo_abs) {
            Ok(reading) if !reading.moved_and_edited().is_empty() => reading,
            Ok(_) => continue,
            Err(e) => {
                unread = true;
                fix_errors.push(format!("{location}: drift could not be read: {e:#}"));
                continue;
            }
        };
        held_checkouts.insert(repo_abs.clone());
        let elsewhere = held
            .homes
            .elsewhere(workweave)
            .map(std::path::Path::to_path_buf);
        let place = |judging: crate::diagnosis::Judging| match &elsewhere {
            Some(home) => judging.elsewhere(home),
            None => judging,
        };
        if vcs.unmerged_paths(repo_abs).is_ok_and(|u| !u.is_empty()) {
            held.refused.add(crate::diagnosis::judge_unresolved(
                vcs,
                location,
                repo_abs,
                elsewhere.as_deref(),
            ));
            continue;
        }
        let judging = crate::diagnosis::held(vcs, location, repo_abs, &reading).checkout(repo_abs);
        let carries = !reading.in_the_way(repo_abs).is_empty()
            || !vcs
                .uncompared_paths(repo_abs, &reading.moved_and_edited())
                .is_ok_and(|uncompared| uncompared.is_empty());
        merged_any |= held.resolving.is_some();
        let Some(resolution) = held.resolving.filter(|_| elsewhere.is_none() && !carries) else {
            held.refused.add(place(judging).judge(vcs));
            continue;
        };
        let outcome = self::merge_edited_drift(vcs, repo_abs, &reading, resolution);
        if carried(location, repo_abs, outcome, &mut held.refused, fix_errors) {
            held_checkouts.remove(repo_abs);
        }
    }
    held.merge_found = merged_any;
    held.unread = unread;
    HeldPass {
        held: held_checkouts,
        merged: merged_checkouts,
    }
}

/// `violations` without the drift findings of a checkout the pass held,
/// which its refusal names instead, and with a merged checkout's drift
/// findings read again.
fn drift_findings_standing(
    vcs: &dyn crate::vcs::Vcs,
    violations: Vec<CheckViolation>,
    repo_locations: &std::collections::HashMap<(Option<WorkweaveId>, RepoPath), std::path::PathBuf>,
    pass: &HeldPass,
) -> Vec<CheckViolation> {
    let after_merge: std::collections::HashMap<
        std::path::PathBuf,
        (Vec<RecordDriftKind>, Vec<WorkingTreeDriftKind>),
    > = pass
        .merged
        .iter()
        .map(|at| {
            let conflicted = vcs.unmerged_paths(at).is_ok_and(|u| !u.is_empty());
            (
                at.clone(),
                read_drift(vcs, at)
                    .ok()
                    .filter(|_| !conflicted)
                    .map(|reading| drift_findings(&reading))
                    .unwrap_or_default(),
            )
        })
        .collect();
    let standing = |workweave: &Option<WorkweaveId>, repo: &RepoPath| {
        let at = repo_locations.get(&(workweave.clone(), repo.clone()));
        (
            at.is_some_and(|at| pass.held.contains(at)),
            at.and_then(|at| after_merge.get(at)),
        )
    };
    violations
        .into_iter()
        .filter(|v| match v {
            CheckViolation::RecordDrift {
                workweave,
                repo,
                kind,
            } => match standing(workweave, repo) {
                (true, _) => false,
                (_, Some((index, _))) => index.contains(kind),
                _ => true,
            },
            CheckViolation::WorkingTreeDrift {
                workweave,
                repo,
                kind,
            } => match standing(workweave, repo) {
                (true, _) => false,
                (_, Some((_, files))) => files.contains(kind),
                _ => true,
            },
            _ => true,
        })
        .collect()
}

fn apply_finding_repairs(
    ctx: &crate::workspace::WorkspaceContext,
    world: &DoctorWorld,
    violations: Vec<CheckViolation>,
    repo_locations: &std::collections::HashMap<(Option<WorkweaveId>, RepoPath), std::path::PathBuf>,
    release_unreadable_claims: Option<crate::cli::consent::ReleaseUnreadableClaimsConsent>,
    held: &mut HeldDrift,
    fix_errors: &mut Vec<String>,
) -> Vec<CheckViolation> {
    let vcs = world.vcs.as_ref();

    let location_of = |workweave: &Option<WorkweaveId>, repo: &RepoPath| match workweave {
        Some(ww) => format!("{ww}/{repo}"),
        None => format!("{repo}"),
    };

    let (registrations, violations): (Vec<_>, Vec<_>) = violations.into_iter().partition(|v| {
        matches!(
            v,
            CheckViolation::StaleWorktreeRegistration {
                sub_kind: RegistrationDirectoryKind::DirectoryGone,
                ..
            }
        )
    });

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
                    outln!(
                        "[fixed] core: stale-worktree-registration for {}: pruned",
                        location_of(workweave, repo)
                    );
                    true
                }
                Ok(false) => false,
                Err(e) => {
                    fix_errors.push(format!("state-hygiene --fix failed: {e:#}"));
                    true
                }
            },
            None => false,
        };
        if !repaired {
            kept.push(v);
        }
    }

    let pass = settle_held_drift(vcs, &violations, repo_locations, held, fix_errors);
    let held_checkouts = pass.held.clone();
    let mut refreshed_checkouts: std::collections::HashSet<std::path::PathBuf> =
        pass.merged.clone();
    let violations = drift_findings_standing(vcs, violations, repo_locations, &pass);
    for v in violations {
        if matches!(v.fix_disposition(), FixDisposition::ReportOnly) {
            kept.push(v);
            continue;
        }
        let repaired = match &v {
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
                    outln!(
                        "[fixed] core: re-pointed dangling parent of {} to primary",
                        workweave_dir.display()
                    );
                    true
                }
                Ok(false) => true,
                Err(e) => {
                    fix_errors.push(format!("dangling-parent --fix failed: {e:#}"));
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
                let outcome = fix_stale_registry_entry(ctx.primary_path(), project, workweave_name);
                match outcome {
                    Ok(StaleEntryFix::MarkerRestored) => {
                        outln!(
                            "[fixed] core: restored the marker of workweave `{}` at {} in \
                             project `{}` from the index and the worktree relation",
                            workweave_name,
                            recorded_path.display(),
                            project
                        );
                        true
                    }
                    Ok(StaleEntryFix::Pruned) => {
                        outln!(
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
                             failed: {e:#}",
                            workweave_name, project
                        ));
                        false
                    }
                }
            }

            CheckViolation::UnreadableWorkweaveIndex {
                project,
                index_path,
                ..
            } if *index_path
                == crate::workweave_index::legacy_index_path(ctx.primary_path(), project)
                    .reported() =>
            {
                false
            }
            CheckViolation::UnreadableWorkweaveIndex { project, .. } => {
                match rebuild_workweave_index(ctx.primary_path(), project) {
                    Ok(Some(names)) => {
                        outln!(
                            "[fixed] core: unreadable-workweave-index of project `{project}`: \
                             rebuilt it listing {} workweave(s) found on disk{}{}; the basedir \
                             it recorded is not rebuilt, and a workweave outside the scanned \
                             containers is listed again by the next rwv run inside it",
                            names.len(),
                            if names.is_empty() { "" } else { ": " },
                            names
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(", "),
                        );
                        true
                    }
                    Ok(None) => true,
                    Err(e) => {
                        fix_errors.push(format!(
                            "workweave-index --fix failed: rebuild of project `{project}`'s \
                             index failed: {e:#}"
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
                let outcome = fix_unregistered_workweave(
                    ctx.primary_path(),
                    project,
                    workweave_name,
                    workweave_dir,
                );
                match outcome {
                    Ok(()) => {
                        outln!(
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
                             failed: {e:#}",
                            workweave_name,
                            workweave_dir.display()
                        ));
                        false
                    }
                }
            }

            CheckViolation::RecordDrift {
                workweave,
                repo,
                kind: RecordDriftKind::SafeToFix,
            }
            | CheckViolation::WorkingTreeDrift {
                workweave,
                repo,
                kind: WorkingTreeDriftKind::SafeToFix,
            } => match repo_locations.get(&(workweave.clone(), repo.clone())) {
                Some(repo_abs) if held_checkouts.contains(repo_abs) => false,
                Some(repo_abs) if !refreshed_checkouts.insert(repo_abs.clone()) => true,
                Some(repo_abs) => {
                    let location = location_of(workweave, repo);
                    let refreshed = read_drift(vcs, repo_abs)
                        .and_then(|reading| refresh_drift(vcs, repo_abs, &reading));
                    match refreshed {
                        Ok(outcome) => {
                            match &outcome.refreshed {
                                Some(refreshed) => {
                                    held.refreshed = true;
                                    outln!(
                                        "[fixed] core: drift in {location}: {}",
                                        refreshed.describe()
                                    )
                                }
                                None if outcome.left_in_place.is_none() => outln!(
                                    "[fixed] core: drift in {location}: nothing left to refresh"
                                ),
                                None => {}
                            }
                            if let Some(left) =
                                outcome.describe_left_in_place(vcs, repo_abs, "rwv doctor --fix")
                            {
                                fix_errors.push(format!("{location}: {left}"));
                            }
                            if let Some(unrecorded) = outcome.describe_unrecorded() {
                                fix_errors.push(format!("{location}: {unrecorded}"));
                            }
                        }
                        Err(e) => {
                            fix_errors.push(format!("{location}: drift refresh failed: {e:#}"))
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
                key,
                sub_kind,
            } => match repo_locations.get(&(workweave.clone(), repo.clone())) {
                Some(repo_abs) => match fix_state_hygiene(vcs, &v, repo_abs) {
                    Ok(true) => {
                        let done = match sub_kind {
                            OrphanedSavepointKind::Redundant => "dropped",
                            OrphanedSavepointKind::Live => {
                                "kept as a backup (`rwv backup list`) and dropped"
                            }
                        };
                        outln!(
                            "[fixed] core: orphaned-savepoint for {}: {done} op_id={key}",
                            location_of(workweave, repo)
                        );
                        true
                    }
                    Ok(false) => false,
                    Err(e) => {
                        fix_errors.push(format!("state-hygiene --fix failed: {e:#}"));
                        true
                    }
                },
                None => false,
            },

            CheckViolation::ClaimOwnerUnreachable {
                claim_dir,
                op_id,
                recorded_owner,
            } => {
                let claim = op_id
                    .parse()
                    .map(|op| crate::landing_claim::UnreachableClaim {
                        dir: claim_dir.clone(),
                        op,
                        owner: recorded_owner.clone(),
                    });
                match claim.map_err(anyhow::Error::from).and_then(|claim| {
                    crate::landing_claim::release_unreachable(&claim, ctx.active_path())
                }) {
                    Ok(true) => {
                        outln!(
                            "[fixed] core: claim-owner-unreachable at {}: released the claim of \
                             op {op_id}",
                            claim_dir.display()
                        );
                        true
                    }
                    Ok(false) => false,
                    Err(e) => {
                        fix_errors.push(format!("claim release failed: {e:#}"));
                        true
                    }
                }
            }

            CheckViolation::ClaimUnreadable { file, seen } => match release_unreadable_claims
                .as_ref()
                .filter(|consent| consent.admits(file, ctx.active_project()))
            {
                None => false,
                Some(_) => match crate::landing_claim::release_unreadable(
                    &crate::landing_claim::UnreadableClaim {
                        file: file.clone(),
                        seen: seen.clone(),
                    },
                ) {
                    Ok(true) => {
                        outln!(
                            "[fixed] core: claim-unreadable at {}: released it ({}); {}",
                            file.display(),
                            unreadable_claim_text(seen),
                            if crate::landing_claim::is_takeover_guard_file(file) {
                                "a takeover by an rwv of another release that held it no longer \
                                 guards its removal of the claim beside it"
                            } else {
                                "a landing of another rwv release that held it has lost its claim"
                            }
                        );
                        true
                    }
                    Ok(false) => false,
                    Err(e) => {
                        fix_errors.push(format!("unreadable claim release failed: {e:#}"));
                        false
                    }
                },
            },

            // Operates on the lease's own workspace dir, so no repo lookup.
            CheckViolation::DeadOpLease {
                workspace_dir,
                op_id,
                ..
            } => match fix_state_hygiene(vcs, &v, workspace_dir) {
                Ok(true) => {
                    outln!(
                        "[fixed] core: dead-op-lease for {}: removed lease (op_id={op_id})",
                        workspace_dir.display()
                    );
                    true
                }
                Ok(false) => false,
                Err(e) => {
                    fix_errors.push(format!("state-hygiene --fix failed: {e:#}"));
                    true
                }
            },

            CheckViolation::UnparseableProject {
                project,
                manifest_path,
                sub_kind: UnparseableProjectKind::PreSchema,
                ..
            } => {
                match fix_pre_schema_manifest(world.project_vcs.as_ref(), manifest_path) {
                    Ok(PreSchemaFix::Rewritten { committed }) => {
                        let note = if committed {
                            ", committing only that rewrite"
                        } else {
                            "; HEAD holds none, so there is nothing to commit"
                        };
                        outln!("[fixed] core: {project}: rewrote the member-entry keys in rwv.toml{note}");
                        true
                    }
                    Ok(PreSchemaFix::Withheld(reason)) => {
                        fix_errors.push(format!("{project}: rwv.toml not rewritten: {reason}"));
                        false
                    }
                    Err(e) => {
                        fix_errors.push(format!("{project}: rwv.toml not rewritten: {e:#}"));
                        false
                    }
                }
            }

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repair {
    Report,
    Apply,
}

impl Repair {
    /// The repair `--fix` asks for.
    pub fn from_fix_flag(fix: bool) -> Self {
        if fix {
            Self::Apply
        } else {
            Self::Report
        }
    }
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
        let project_dir = crate::workspace::project_dir(&world.workspace_dir, &project.name);
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
        source: IssueSource::Core,
        severity: Severity::Warning,
        message: format!(
            "{} may no longer match this checkout — {cause}. Run `rwv materialize` \
             to re-derive it",
            finding.generated
        )
        .into(),
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
    ctx_base: &IntegrationContextBase,
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
                        ctx_base
                            .output_dir()
                            .join(crate::path_spelling::native_relative(path.name()))
                            .display()
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
                source: IssueSource::integration(&found.integration),
                severity: Severity::Warning,
                message: format!(
                    "{} is disabled for project `{project}`, so nothing here is its \
                     to keep, but content it authored is still on disk: {described}{also}. \
                     Run `rwv materialize` to remove it. `rwv doctor --fix` will not — \
                     removal is not a repair, and disabling an integration is one \
                     character in rwv.toml",
                    found.integration
                )
                .into(),
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
        let ctx_base = IntegrationContextBase::new(
            &world.session,
            &project.name,
            &detection_cache,
            project.manifest.workweave.as_ref(),
        );
        let check_issues = run_checks(&integrations, &project.manifest, &ctx_base);
        let malformed_per_check: std::collections::BTreeSet<IssueSource> = check_issues
            .iter()
            .filter(|issue| issue.kind == IssueKind::MalformedSettings)
            .map(|issue| issue.source.clone())
            .collect();
        issues.extend(check_issues);
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
            verify_issues.into_iter().partition(Issue::fix_permitted);
        // Withholding is per project, because the repair is: activation runs
        // every managed file's hooks, and a hook re-attests what it produces —
        // so a user-held finding anywhere in the project is content this
        // repair settles without being told which way to settle it. The
        // per-finding `safe_to_fix` keeps `--fix` off the finding itself and
        // says nothing about a repair entered for a different one; the drift
        // refusal that would catch it lives in activation's materialize mode,
        // and this is its intent mode.
        let user_held_content = !user_held_issues.is_empty();
        // The block `check()` already reported is one finding, not two.
        issues.extend(user_held_issues.into_iter().filter(|issue| {
            !(issue.kind == IssueKind::MalformedSettings
                && malformed_per_check.contains(&issue.source))
        }));
        if fix && !fixable_issues.is_empty() && !user_held_content {
            // The repair primitive must be pointed at the same weave the
            // detector scanned. `activate_intent` targets primary
            // unconditionally, so from a workweave it would rewrite the
            // PRIMARY project's managed files — `activate_intent_at` takes the
            // weave dir, and `workspace_dir` is `ctx.active_path()`.
            match crate::activate::activate_intent_at(&project.name, workspace_dir) {
                Ok(_) => outln!(
                    "[fixed] core: regenerated integration content for project `{}` (drift detected)",
                    project.name
                ),
                Err(e) => issues.push(Issue {
                    kind: IssueKind::CoreFinding,
                    source: IssueSource::Core,
                    severity: Severity::Error,
                    message: format!(
                        "doctor --fix: failed to regenerate integration content for `{}`: {e:#}",
                        project.name
                    )
                    .into(),
                    safe_to_fix: true,
                }),
            }
        } else {
            if fix && !fixable_issues.is_empty() {
                issues.push(Issue {
                    kind: IssueKind::CoreFinding,
                    source: IssueSource::Core,
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
                    )
                    .into(),
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
        let surfacing_issues = crate::activate::verify_surfacing(
            world.project_vcs.as_ref(),
            workspace_dir,
            &project.name,
            &project.manifest,
        );
        let (surf_fixable, surf_user_held): (Vec<_>, Vec<_>) =
            surfacing_issues.into_iter().partition(Issue::fix_permitted);
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
                world.project_vcs.as_ref(),
                workspace_dir,
                &project.name,
                &project.manifest,
                crate::activate::SurfacingMode::Repair,
            ) {
                Ok(()) => outln!(
                    "[fixed] core: re-surfaced symlinks for project `{}` (missing/mis-resolved surfacing)",
                    project.name
                ),
                Err(e) => issues.push(Issue {
                    kind: IssueKind::CoreFinding,
                    source: IssueSource::Core,
                    severity: Severity::Error,
                    message: format!(
                        "doctor --fix: failed to re-surface symlinks for `{}`: {e:#}",
                        project.name
                    )
                    .into(),
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
/// Kind names are the wire spellings — the `kind` tag each violation and
/// each integration issue serializes under in `rwv doctor --json`, the same
/// names the per-class count lines and the published schema carry. The two
/// registers share no name.
///
/// **An unknown name refuses, naming the valid set.** A filter that
/// silently matched nothing would render an empty report, and an empty
/// doctor report reads as "clean" — the one thing a typo must never
/// produce. The valid set is derived from the wire types' own JSON schemas
/// ([`ViolationOutput`] and [`IssueKindOutput`] via schemars) rather than
/// maintained beside them, so the refusal message cannot drift from what
/// `--json` actually emits.
///
/// **The filtered view is the drill-down.** Kinds whose classes normally
/// collapse to a per-class count line render itemized under the filter —
/// `--kind` exists so triage does not require `--json | jq`, so it shows
/// the records the count line summarized. An issue is in the view when its
/// own kind is named, and the exit code reflects the filtered view: a
/// finding outside the named kinds does not fail a `--kind` run.
pub struct KindFilter {
    kinds: std::collections::BTreeSet<String>,
}

impl KindFilter {
    /// Every `kind` name `rwv doctor --json` can emit, sorted — read off
    /// the wire types' schemas, which are generated from the serialized
    /// types and therefore are the register.
    pub fn valid_kinds() -> Vec<String> {
        let mut kinds = Self::violation_kinds();
        kinds.extend(Self::issue_kinds());
        kinds.sort();
        kinds
    }

    /// The `kind` names an integration issue serializes under: the unit
    /// variants of [`IssueKindOutput`] as strings, a data-carrying variant as
    /// its single key.
    pub fn issue_kinds() -> Vec<String> {
        let schema = schemars::schema_for!(IssueKindOutput);
        let variants = schema
            .schema
            .subschemas
            .as_ref()
            .and_then(|s| s.one_of.clone())
            .unwrap_or_default();
        let mut kinds: Vec<String> = variants
            .iter()
            .flat_map(|v| {
                let schemars::schema::Schema::Object(obj) = v else {
                    return Vec::new();
                };
                let named = obj
                    .enum_values
                    .iter()
                    .flatten()
                    .filter_map(|value| value.as_str().map(String::from));
                let keyed = obj.object.iter().flat_map(|o| o.properties.keys().cloned());
                named.chain(keyed).collect()
            })
            .collect();
        kinds.sort();
        kinds
    }

    fn violation_kinds() -> Vec<String> {
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
                crate::refuse!(
                    RefusalKind::UnknownFindingKind,
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
        self.kinds.contains(v.kind().to_string().as_str())
    }

    /// Whether `issue` belongs to one of the named kinds.
    fn admits_issue(&self, issue: &Issue) -> bool {
        self.kinds.contains(issue.kind.tag().as_str())
    }
}

/// Each violation kind that carries a `sub_kind`, with every tag that field
/// can hold, read off the wire type's schema as [`KindFilter`] reads the kinds.
pub fn finding_sub_kinds() -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>>
{
    let root = serde_json::to_value(schemars::schema_for!(ViolationOutput))
        .expect("a derived schema serializes");
    let tags = |def: &serde_json::Value| -> std::collections::BTreeSet<String> {
        let variants = match def["oneOf"].as_array() {
            Some(variants) => variants.iter().collect(),
            None => vec![def],
        };
        variants
            .into_iter()
            .flat_map(|variant| match variant["enum"].as_array() {
                Some(values) => values.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
                None => variant["required"]
                    .as_array()
                    .filter(|keys| keys.len() == 1)
                    .map(|keys| keys.iter().filter_map(|k| k.as_str()).collect())
                    .unwrap_or_default(),
            })
            .map(str::to_owned)
            .collect()
    };
    root["oneOf"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|variant| {
            let kind = variant["properties"]["kind"]["enum"][0].as_str()?;
            let sub_kind = &variant["properties"]["sub_kind"];
            let reference = sub_kind["$ref"]
                .as_str()
                .or(sub_kind["allOf"][0]["$ref"].as_str())?;
            let definition = &root["definitions"][reference.rsplit('/').next()?];
            Some((kind.to_owned(), tags(definition)))
        })
        .collect()
}

/// What a `rwv doctor` run decided: whether the process exits non-zero.
///
/// The three runners return it and dispatch reads the exit from it, so no
/// caller holds a bool whose `true` it must remember means "exit 1".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Exit 0.
    Pass,
    /// Exit 1.
    Fail,
}

/// How much of the weave a doctor run looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The active project: pins, dangling references and integration
    /// hooks are scoped to it, and orphan detection is skipped (a repo absent
    /// from the active project may belong to another). A workspace with no
    /// active project has nothing to narrow to and is scanned weave-wide.
    ActiveProject,
    /// `--all`: every project, orphan detection across them included.
    Weave,
}

impl Scope {
    /// The scope `--all` asks for.
    pub fn from_all_flag(all: bool) -> Self {
        if all {
            Self::Weave
        } else {
            Self::ActiveProject
        }
    }
}

/// The one place `rwv doctor` decides whether to exit non-zero, text and
/// `--json` alike.
///
/// `severities` are those of the findings the run reports. Without `--kind`,
/// only an error-severity finding fails the run; a warning-severity one
/// passes. `--kind` has already narrowed the findings to what the filter
/// admits, so any survivor fails the run regardless of severity: that is what
/// lets a CI gate escalate a warning-severity kind such as `tool-missing` by
/// naming it.
fn doctor_verdict(
    kind_filter: Option<&KindFilter>,
    severities: impl IntoIterator<Item = crate::integration::Severity>,
) -> Verdict {
    let mut severities = severities.into_iter();
    let failed = match kind_filter {
        Some(_) => severities.next().is_some(),
        None => severities.any(|severity| severity == crate::integration::Severity::Error),
    };
    if failed {
        Verdict::Fail
    } else {
        Verdict::Pass
    }
}

/// Under [`Repair::Apply`], the repairable subset is remediated in place before
/// the report is rendered, so a repaired workspace reports healthy.
///
/// Both of its inputs are collected elsewhere and shared with
/// [`run_check_json`]: `collect_doctor_violations` for the core findings and
/// `collect_doctor_issues` for the integrations'. The only thing this
/// function raises itself is a `--fix` arm's own failure, which `--json` has no
/// `--fix` to produce.
///
/// [`Scope`] says how much of the weave is checked.
///
/// Returns [`Verdict::Fail`] when `kind_filter` is absent and some finding is
/// error-severity, or when it is present and it admits any finding regardless
/// of severity; [`Verdict::Pass`] otherwise. [`run_check_json`] follows the
/// same contract.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
///
pub fn run_check(
    ctx: &crate::workspace::WorkspaceContext,
    repair: Repair,
    scope: Scope,
    abandon_lost_pins: Option<crate::cli::consent::AbandonLostPinsConsent>,
    release_unreadable_claims: Option<crate::cli::consent::ReleaseUnreadableClaimsConsent>,
    edited_drift: Option<crate::cli::consent::EditedDriftConsent>,
    kind_filter: Option<&KindFilter>,
) -> anyhow::Result<Verdict> {
    use crate::integration::Severity;

    // The P2 gate: a binary whose release removed migratory arms refuses a
    // weave whose recorded floor predates them, naming the bridge version.
    // A no-op while no removal has named a requirement.
    crate::health_floor::enforce(ctx.primary_path())?;

    let mut fix_errors: Vec<String> = Vec::new();
    let mut held = HeldDrift {
        resolving: edited_drift
            .filter(|_| repair == Repair::Apply)
            .map(crate::cli::consent::EditedDriftConsent::resolution),
        refused: crate::diagnosis::Report::default(),
        merge_found: false,
        unread: false,
        refreshed: false,
        homes: Homes::default(),
    };
    let fix = repair == Repair::Apply;

    if fix {
        apply_prelude_repairs(ctx, &mut fix_errors);
        let project_scope = match scope {
            Scope::Weave => None,
            Scope::ActiveProject => ctx.active_project(),
        };
        repair_member_gaps(ctx, project_scope, abandon_lost_pins, &mut fix_errors);
        repair_standalone_members(ctx, project_scope, &mut fix_errors);
        repair_uninitialized_submodules(ctx, project_scope);
    }

    let world = load_doctor_world(ctx, scope)?;

    if fix {
        apply_workspace_repairs(ctx, &world, &mut fix_errors);
    }

    let mut findings = collect_doctor_violations(ctx, &world, ScanProgress::Heartbeat);
    held.homes = Homes {
        here: ctx.active_path().to_path_buf(),
        primary: world.workspace_dir.clone(),
        workweaves: findings.workweave_dirs.clone(),
    };

    if !fix {
        let violations = std::mem::take(&mut findings.violations);
        let pass = settle_held_drift(
            world.vcs.as_ref(),
            &violations,
            &findings.repo_locations,
            &mut held,
            &mut fix_errors,
        );
        findings.violations = drift_findings_standing(
            world.vcs.as_ref(),
            violations,
            &findings.repo_locations,
            &pass,
        );
    }
    if fix {
        let violations = std::mem::take(&mut findings.violations);
        let violations = apply_finding_repairs(
            ctx,
            &world,
            violations,
            &findings.repo_locations,
            release_unreadable_claims,
            &mut held,
            &mut fix_errors,
        );
        findings.violations = repair_member_permissions(
            ctx,
            &world.input.projects,
            world.project_scope(),
            violations,
            &mut fix_errors,
        );
    }

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
    let violations_all_report_only = findings
        .violations
        .iter()
        .all(|v| matches!(v.fix_disposition(), FixDisposition::ReportOnly));

    let issue_repair = match kind_filter {
        Some(_) => Repair::Report,
        None => repair,
    };
    let findings = findings
        .with_issues(collect_doctor_issues(&world, issue_repair))
        .admitting(kind_filter);
    let lines = text_report(findings, fix_errors, &world.workspace_dir);

    for ReportLine { token, issue } in &lines {
        let severity = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        outln!("[{severity}: {token}] {}: {}", issue.source, issue.message);
    }
    let verdict = doctor_verdict(kind_filter, lines.iter().map(|line| line.issue.severity));

    // The P1 record: a clean weave-wide unfiltered run advances the health
    // floor. Weave-wide because the floor licenses arm removal for the
    // whole weave and a scoped run proves nothing beyond its project;
    // unfiltered because `--kind` narrows what was even looked at. Clean
    // means no error-severity finding and no violation rwv can still
    // repair; report-only warnings do not block, per the classification
    // where `violations_all_report_only` is computed. Best-effort: a
    // floor is a record, not a precondition of the run that earned it.
    if scope == Scope::Weave
        && kind_filter.is_none()
        && violations_all_report_only
        && verdict == Verdict::Pass
    {
        if let Err(e) =
            crate::health_floor::record_clean_run(ctx.primary_path(), world.vcs.as_ref())
        {
            eprintln!("warning: could not record the health floor: {e:#}");
        }
    }

    held.refused.into_result()?;
    if let (true, Some(resolution), false, false, false) = (
        fix,
        held.resolving,
        held.merge_found,
        held.unread,
        held.refreshed,
    ) {
        let flag = match resolution {
            crate::vcs::EditResolution::Merge => "--merge-edited-drift",
            crate::vcs::EditResolution::SetAside => "--set-aside-edited-drift",
        };
        crate::refuse!(
            crate::refusal::RefusalKind::InapplicableFlag,
            "{flag}: no checkout here holds a file both moved by its branch and edited, so \
             there is no edit to resolve against a moved tip; where no move of a checkout's \
             branch under it is shown, its differences are edits"
        );
    }
    Ok(verdict)
}

/// The payload for `rwv doctor --json`: every finding the run holds, one
/// record each, with the resolved workspace context beside them.
pub fn build_doctor_json(
    findings: DoctorFindings,
    workspace_dir: &Path,
    resolution: Option<Resolution>,
    plugins: Vec<crate::plugins::PluginRecord>,
    advisories: Vec<crate::workspace::AdvisoryOutput>,
) -> DoctorJsonOutput {
    let workweave_dirs = &findings.workweave_dirs;
    DoctorJsonOutput {
        schema_url: DOCTOR_SCHEMA_URL.to_owned(),
        violations: findings
            .violations
            .into_iter()
            .map(|v| ViolationOutput::from_violation(v, workspace_dir, workweave_dirs))
            .collect(),
        issues: findings
            .issues
            .into_iter()
            .map(IssueOutput::from_issue)
            .collect(),
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
    project_vcs: Box<dyn crate::vcs::Vcs>,
    active_project: Option<crate::manifest::ProjectName>,
    scope: Scope,
    input: CheckInput,
    unparseable_projects: Vec<UnparseableProject>,
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
    fn project_scope(&self) -> Option<&ProjectName> {
        match self.scope {
            Scope::Weave => None,
            Scope::ActiveProject => self.active_project.as_ref(),
        }
    }
}

struct UnparseableProject {
    project: crate::manifest::ProjectName,
    manifest_path: PathBuf,
    message: String,
    sub_kind: UnparseableProjectKind,
}

impl UnparseableProject {
    fn new(
        project: &crate::manifest::ProjectName,
        manifest_path: PathBuf,
        failure: &anyhow::Error,
    ) -> Self {
        Self {
            project: project.clone(),
            manifest_path,
            message: format!("{failure:#}"),
            sub_kind: UnparseableProjectKind::of(failure),
        }
    }
}

fn load_doctor_world(
    ctx: &crate::workspace::WorkspaceContext,
    scope: Scope,
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
            Err(e) => head_read_failures.push((repo_path.clone(), format!("{e:#}"))),
        }
    }

    let projects_dir_path = projects_dir(&workspace_dir);
    let projects_dir_read_error = match std::fs::read_dir(&projects_dir_path) {
        Ok(_) => None,
        // A workspace with no `projects/` at all yet (before the first `rwv
        // add`) is a different, unremarkable state — only a directory that
        // exists and refuses to be listed is the finding.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some((projects_dir_path, format!("{e:#}"))),
    };

    let mut projects = Vec::new();
    let mut known_repos = BTreeSet::new();
    let mut lock_resolve_failures: Vec<(crate::manifest::ProjectName, RepoPath)> = Vec::new();
    let mut unparseable_projects: Vec<UnparseableProject> = Vec::new();
    let mut resolved_locks: std::collections::HashMap<
        crate::manifest::ProjectName,
        crate::manifest::ResolvedLockFile,
    > = std::collections::HashMap::new();

    let project_scan = crate::workspace::scan_projects(&workspace_dir);

    for name in &project_scan.projects {
        let dir = project_dir(&workspace_dir, name);
        let manifest_path = dir.join(Manifest::FILE_NAME);

        if scope == Scope::ActiveProject {
            if let Some(ref active) = active_project {
                if name != active {
                    if let Err(e) = Manifest::from_path(&manifest_path) {
                        unparseable_projects.push(UnparseableProject::new(name, manifest_path, &e));
                    }
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
                        if !crate::workweave::is_dangling_link(
                            &workspace_dir.join(unresolved.as_path()),
                        ) {
                            lock_resolve_failures.push((project.name.clone(), unresolved));
                        }
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
                unparseable_projects.push(UnparseableProject::new(name, manifest_path, &e));
            }
        }
    }

    // Orphan detection is weave-wide by construction: a repo is orphaned only
    // if it belongs to *no* project, which a partial load cannot establish.
    let check_orphans = scope == Scope::Weave || active_project.is_none();

    let unknown_segment_repos =
        crate::workspace::scan_unknown_segment_repos(&workspace_dir, vcs.as_ref());

    let clones_at_placement = projects
        .iter()
        .flat_map(|project| {
            project
                .manifest
                .iter_entries()
                .filter(|(repo, _)| !session.repos_on_disk().contains(repo))
                .filter_map(|(repo, entry)| {
                    clone_at_placement(&workspace_dir, &project.manifest, repo, entry)
                        .map(|placed| ((project.name.clone(), repo.clone()), placed))
                })
                .collect::<Vec<_>>()
        })
        .collect();

    let dangling_links = projects
        .iter()
        .flat_map(|project| project.manifest.iter_repo_paths())
        .filter(|repo| crate::workweave::is_dangling_link(&workspace_dir.join(repo.as_path())))
        .cloned()
        .collect();

    let input = CheckInput {
        known_repos,
        repos_on_disk: session.repos_on_disk().to_vec(),
        dangling_links,
        clones_at_placement,
        unknown_segment_repos,
        projects,
        head_revisions,
        resolved_locks,
        check_orphans,
    };

    Ok(DoctorWorld {
        workspace_dir,
        session,
        vcs,
        project_vcs: crate::vcs::project_vcs(),
        active_project,
        scope,
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

/// What one `rwv doctor` run found, in the scope it ran under and through the
/// kind filter it was given: the only input either renderer takes.
///
/// Its fields are private, so nothing outside this module can build one or
/// change what it holds; a renderer cannot be handed findings some other
/// collection produced.
///
/// `repo_locations` resolves a `(workweave, repo)` pair from a violation back
/// to the worktree it was observed in, which is what the text renderer's
/// `--fix` arms need to act on a finding they did not scan for themselves.
pub struct DoctorFindings {
    violations: Vec<CheckViolation>,
    issues: Vec<Issue>,
    filtered: bool,
    workweave_dirs: std::collections::HashMap<WorkweaveId, std::path::PathBuf>,
    repo_locations: std::collections::HashMap<(Option<WorkweaveId>, RepoPath), std::path::PathBuf>,
}

impl DoctorFindings {
    fn with_issues(mut self, issues: Vec<Issue>) -> Self {
        self.issues.extend(issues);
        self
    }

    /// Narrowed to what `kind_filter` admits, on both channels. Each record
    /// that survives is unchanged.
    fn admitting(mut self, kind_filter: Option<&KindFilter>) -> Self {
        if let Some(filter) = kind_filter {
            self.violations.retain(|v| filter.admits(v));
            self.issues.retain(|issue| filter.admits_issue(issue));
            self.filtered = true;
        }
        self
    }
}

/// The text report's lines for `findings`: its violations, collapsed by class
/// unless a kind filter narrowed them, then `fix_errors`, then its issues.
///
/// The filtered view is the drill-down: the admitted kinds render itemized,
/// bypassing the per-class count collapse, because `--kind` is what replaces
/// `--json | jq` for triage and must show the records, not the count line
/// that pointed there.
fn text_report(
    findings: DoctorFindings,
    fix_errors: Vec<String>,
    workspace_dir: &Path,
) -> Vec<ReportLine> {
    use crate::integration::Severity;
    let workweave_dirs = &findings.workweave_dirs;
    let token_of = |v: &CheckViolation| {
        ViolationOutput::from_violation(v.clone(), workspace_dir, workweave_dirs).token()
    };
    let mut lines = if findings.filtered {
        itemized_violations_to_issues(findings.violations, token_of)
    } else {
        violations_to_issues(findings.violations, token_of)
    };
    let issues = fix_errors
        .into_iter()
        .map(|message| Issue {
            kind: IssueKind::CoreFinding,
            source: IssueSource::Core,
            severity: Severity::Error,
            message: message.into(),
            safe_to_fix: true,
        })
        .chain(findings.issues);
    lines.extend(issues.map(|issue| ReportLine {
        token: issue.kind.tag(),
        issue,
    }));
    lines
}

/// One line of the text report: a finding, and the name `rwv explain` takes
/// for it — the `kind` and `sub_kind` `--json` carries, in one token.
struct ReportLine {
    token: String,
    issue: Issue,
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

    let member_gaps = scan_member_gaps(ctx, project_scope);
    violations.extend(member_gaps.iter().map(MemberGap::finding));
    violations.extend(
        member_slots(ctx, &input.projects, project_scope)
            .iter()
            .filter_map(member_permissions_finding),
    );
    violations.extend(scan_role_disagreements(ctx, &input.projects));
    let leftovers = scan_leftover_checkouts(ctx, project_scope, &input.projects);
    violations.retain(|v| match v {
        CheckViolation::OrphanedClone { path } => !leftovers.iter().any(|l| {
            matches!(l, CheckViolation::WorkweaveDrift { workweave: None, repo, .. } if repo == path)
        }),
        _ => true,
    });
    violations.extend(leftovers);
    violations.extend(scan_locks_pinned_by_name(&input.projects));
    let mut unjudged: Vec<String> = Vec::new();
    violations.extend(scan_history_disagreements(
        ctx,
        &input.projects,
        &mut unjudged,
    ));
    violations.extend(scan_pins(ctx, world, &mut unjudged));

    match primary_pointer(ctx.primary_path()) {
        Some(PointerContent::Selects(active_name)) => {
            let project_dir = project_dir(ctx.primary_path(), &active_name);
            if !project_dir.is_dir() {
                violations.push(CheckViolation::DanglingActiveProject {
                    project: active_name,
                    missing_dir: project_dir,
                });
            }
        }
        Some(PointerContent::Unreadable { path, detail }) => {
            violations.push(CheckViolation::UnreadableActiveProject {
                path: path.reported(),
                detail,
            });
        }
        None => {}
    }

    violations.extend(scan_weave_root_identity(
        ctx.primary_path(),
        ctx.active_path(),
    ));

    for unparseable in &world.unparseable_projects {
        violations.push(CheckViolation::UnparseableProject {
            project: unparseable.project.clone(),
            manifest_path: unparseable.manifest_path.clone(),
            message: unparseable.message.clone(),
            sub_kind: unparseable.sub_kind,
        });
    }

    // Found by walking the directory rather than read off a `Project`: a
    // project whose only manifest is the legacy one never loads.
    for finding in scan_workspace_for_legacy_manifests(workspace_dir) {
        if let Some(active) = project_scope {
            if &finding.project != active {
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
            if &project != active {
                continue;
            }
        }
        match crate::workweave_index::legacy_shape(ctx.primary_path(), &project) {
            Ok(Some((index_path, shape))) => {
                violations.push(CheckViolation::LegacyWorkweaveIndex {
                    project,
                    index_path: index_path.reported(),
                    sub_kind: LegacyWorkweaveIndexKind::of(shape),
                })
            }
            Ok(None) => {}
            // An old-path file that does not parse has no shape to classify;
            // `scan_registry_reconciliation` reports it as unreadable.
            Err(_) => {}
        }
    }

    violations.extend(scan_workweave_tree_integrity(ctx.primary_path()));
    violations.extend(scan_confusable_siblings(
        ctx.primary_path(),
        &input.projects,
    ));
    violations.extend(scan_provenance(ctx, workspace_dir, &input.projects));
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
            let ctx_base = IntegrationContextBase::new(
                &world.session,
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

    violations.extend(scan_uninitialized_submodules_in_workweaves(
        ctx,
        project_scope,
    ));

    violations.extend(scan_clone_topology(
        vcs,
        ctx.primary_path(),
        &input.known_repos,
    ));

    violations.extend(
        scan_alignment(ctx.primary_path(), &input.projects, &mut unjudged)
            .into_iter()
            .filter(|v| match (project_scope, v) {
                (Some(active), CheckViolation::Alignment { project, .. }) => project == active,
                _ => true,
            }),
    );

    // Index-drift and working-tree-drift over every materialized worktree a
    // manifest names, plus — from primary — each workweave's copy. Deduped by
    // `(workweave, abs)` so repos shared between projects cost one round of
    // git subprocesses, and classified in a single pass by `classify_drift`.
    let mut workweave_dirs: std::collections::HashMap<WorkweaveId, std::path::PathBuf> =
        std::collections::HashMap::new();
    let mut index_scan: Vec<(Option<WorkweaveId>, std::path::PathBuf, RepoPath)> = Vec::new();
    let mut scan_seen: std::collections::HashSet<(Option<WorkweaveId>, std::path::PathBuf)> =
        std::collections::HashSet::new();

    let in_workweave = matches!(ctx.checkout, Checkout::Workweave { .. });
    for project in &input.projects {
        for repo_path in project.manifest.iter_repo_paths() {
            let abs = workspace_dir.join(repo_path.as_path());
            if in_workweave && crate::workweave::is_symlinked_member(&abs) {
                continue;
            }
            if abs.exists() && scan_seen.insert((None, abs.clone())) {
                index_scan.push((None, abs, repo_path.clone()));
            }
        }
    }

    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        for (ww, ww_dir) in crate::workweave::list_workweave_dirs(ctx.primary_path()) {
            workweave_dirs.insert(ww.clone(), ww_dir.clone());
            for project in &input.projects {
                for repo_path in project.manifest.iter_repo_paths() {
                    let abs = ww_dir.join(repo_path.as_path());
                    if crate::workweave::is_symlinked_member(&abs) {
                        continue;
                    }
                    if abs.exists() && scan_seen.insert((Some(ww.clone()), abs.clone())) {
                        index_scan.push((Some(ww.clone()), abs, repo_path.clone()));
                    }
                }
            }
        }
    }
    let mut project_repos: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    for project in &input.projects {
        let project_rel = project_rel_path(&project.name);
        let Ok(project_repo_path) = RepoPath::new(project_rel.clone()) else {
            continue;
        };
        let homes = std::iter::once((None, workspace_dir.clone())).chain(
            workweave_dirs
                .iter()
                .map(|(ww, dir)| (Some(ww.clone()), dir.clone())),
        );
        for (ww, dir) in homes {
            let abs = dir.join(&project_rel);
            if abs.is_dir() && scan_seen.insert((ww.clone(), abs.clone())) {
                project_repos.insert(abs.clone());
                index_scan.push((ww, abs, project_repo_path.clone()));
            }
        }
    }
    drop(scan_seen);

    let this_workweave = workweaves_in_scope(ctx, None)
        .into_iter()
        .find(|_| matches!(ctx.checkout, Checkout::Workweave { .. }))
        .map(|(workweave, _, _)| workweave);
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
        // A worktree whose link names a git directory that is gone fails
        // every git command in it, so classifying it would misreport that as
        // live edits. It is reported as missing-canonical-clone when the
        // store it named is gone, and as workweave-drift otherwise.
        if crate::workweave::is_dangling_link(repo_abs) {
            let workweave = ww_label.clone().or_else(|| this_workweave.clone());
            let canonical_path = ctx.primary_path().join(repo_path.as_path());
            if let (Some(workweave), false) = (workweave, canonical_path.exists()) {
                violations.push(CheckViolation::MissingCanonicalClone {
                    workweave,
                    repo: repo_path.clone(),
                    canonical_path,
                });
            }
            continue;
        }

        if crate::git::repo_is_dirty(repo_abs) == Some(false) {
            continue;
        }
        let project_repo = project_repos.contains(repo_abs);
        let (index_kinds, working_tree_kinds) = match read_drift(vcs, repo_abs) {
            Ok(reading) if reading.left.is_none() && project_repo => continue,
            Err(_) if project_repo => continue,
            Ok(reading) => drift_findings(&reading),
            Err(e) => {
                unjudged.push(format!(
                    "{repo_path}: its drift could not be read, so nothing it holds is judged an \
                     edit, and a commit there could record the move's reverse: {e:#}; `{}` \
                     keeps every difference and leaves the checkout at HEAD",
                    vcs.hint(crate::vcs::VcsAction::Stash)
                        .replace("<checkout>", &repo_abs.display().to_string())
                ));
                continue;
            }
        };
        for kind in index_kinds {
            violations.push(CheckViolation::RecordDrift {
                workweave: ww_label.clone(),
                repo: repo_path.clone(),
                kind,
            });
        }
        for kind in working_tree_kinds {
            violations.push(CheckViolation::WorkingTreeDrift {
                workweave: ww_label.clone(),
                repo: repo_path.clone(),
                kind,
            });
        }
    }

    let hygiene_targets: Vec<StateHygieneScanTarget> = index_scan
        .iter()
        .map(|(ww, abs, repo)| StateHygieneScanTarget {
            workweave: ww.clone(),
            abs: abs.clone(),
            repo: repo.clone(),
        })
        .collect();

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
        let project_repo = project_dir(workspace_dir, &project.name);
        if !project_repo.is_dir() {
            continue;
        }
        match vcs.lock_driver_setup(&project_repo) {
            Ok(setup) => violations.extend(MissingLockMergeDriverKind::of(setup).into_iter().map(
                |sub_kind| CheckViolation::MissingLockMergeDriver {
                    project: project.name.clone(),
                    sub_kind,
                },
            )),
            Err(e) => violations.push(CheckViolation::LockMergeDriverUnreadable {
                project: project.name.clone(),
                error: format!("{e:#}"),
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
            error: format!("{:#}", unnameable.error),
        });
    }

    // In a workweave the per-repo scans read the slot itself, so a gap there —
    // nothing at the slot, or a link into the canonical clone — would also
    // surface as their findings, with remedies that do not apply.
    if matches!(ctx.checkout, Checkout::Workweave { .. }) {
        let is_gap = |repo: &RepoPath| member_gaps.iter().any(|gap| &gap.repo == repo);
        violations.retain(|v| match v {
            CheckViolation::DanglingReference { repo, .. } => !is_gap(repo),
            CheckViolation::Pin { repo, sub_kind, .. } if *sub_kind != PinKind::None => {
                !is_gap(repo)
            }
            _ => true,
        });
    }

    let repo_locations = hygiene_targets
        .into_iter()
        .map(|t| ((t.workweave, t.repo), t.abs))
        .collect();

    DoctorFindings {
        violations,
        issues: unjudged
            .into_iter()
            .map(|message| Issue {
                kind: IssueKind::CoreFinding,
                source: IssueSource::Core,
                severity: crate::integration::Severity::Error,
                message: message.into(),
                safe_to_fix: false,
            })
            .collect(),
        filtered: false,
        workweave_dirs,
        repo_locations,
    }
}

/// Run `rwv doctor --json`.
///
/// Emits `{ "$schema": "...", "violations": [...], "issues": [...] }` to
/// stdout, from the same two collection passes [`run_check`] renders as text.
///
/// Returns the [`Verdict`] under the same contract [`run_check`] follows, so
/// a caller scripting against `--json` reads the same signal a human reads off
/// the text report.
///
/// There is no `--fix` on this path, so nothing here repairs and nothing mints
/// a `--fix` failure.
///
/// [`Scope::ActiveProject`] checks only the active project and skips orphan
/// detection; [`Scope::Weave`] is the weave-wide scan.
pub fn run_check_json(
    ctx: &crate::workspace::WorkspaceContext,
    scope: Scope,
    kind_filter: Option<&KindFilter>,
) -> anyhow::Result<Verdict> {
    // The P2 gate, same as the text path. This surface never RECORDS a
    // floor — `--json` is a machine-reading surface and does not mutate
    // weave state; the floor records from the operator's text-mode run.
    crate::health_floor::enforce(ctx.primary_path())?;
    let world = load_doctor_world(ctx, scope)?;
    let findings = collect_doctor_violations(ctx, &world, ScanProgress::Silent)
        .with_issues(collect_doctor_issues(&world, Repair::Report))
        .admitting(kind_filter);
    let verdict = doctor_verdict(
        kind_filter,
        findings
            .violations
            .iter()
            .map(CheckViolation::severity)
            .chain(findings.issues.iter().map(|issue| issue.severity)),
    );
    // Discover `rwv-*` executables on PATH for the inventory. This is
    // reporting only — the presence or absence of plugins never affects the
    // verdict or the doctor exit code.
    let plugins = crate::plugins::discover_plugins(None::<&std::ffi::OsStr>);
    let advisories = stale_generation_findings(&world)
        .iter()
        .map(stale_generation_advisory)
        .collect();
    let payload = build_doctor_json(
        findings,
        &world.workspace_dir,
        Some(ctx.resolution()),
        plugins,
        advisories,
    );
    let out =
        serde_json::to_string_pretty(&payload).context("failed to serialize doctor output")?;
    outln!("{out}");
    Ok(verdict)
}

/// `word` as one shell argument: bare when no shell would split, expand or
/// unescape it, single-quoted otherwise.
fn shell_word(word: &str) -> std::borrow::Cow<'_, str> {
    let bare = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || "/:._-+=@,".contains(c));
    if bare {
        word.into()
    } else {
        format!("'{}'", word.replace('\'', "'\\''")).into()
    }
}

#[cfg(test)]
mod shell_word_tests {
    use super::shell_word;

    #[test]
    fn a_path_a_shell_would_split_is_quoted_whole() {
        assert_eq!(shell_word("/a/b-c_d.e"), "/a/b-c_d.e");
        assert_eq!(shell_word("/my weave/lib"), "'/my weave/lib'");
        assert_eq!(shell_word("/it's"), "'/it'\\''s'");
        assert_eq!(shell_word(r"C:\w\lib"), r"'C:\w\lib'");
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;
    use crate::integration::Severity::{Error, Warning};

    #[test]
    fn without_a_filter_only_an_error_fails_the_run() {
        assert_eq!(doctor_verdict(None, []), Verdict::Pass);
        assert_eq!(doctor_verdict(None, [Warning, Warning]), Verdict::Pass);
        assert_eq!(doctor_verdict(None, [Warning, Error]), Verdict::Fail);
    }

    #[test]
    fn with_a_filter_any_finding_it_admitted_fails_the_run_whatever_its_severity() {
        let filter = KindFilter::new(&["tool-missing".to_owned()]).unwrap();

        assert_eq!(doctor_verdict(Some(&filter), []), Verdict::Pass);
        assert_eq!(doctor_verdict(Some(&filter), [Warning]), Verdict::Fail);
        assert_eq!(doctor_verdict(Some(&filter), [Error]), Verdict::Fail);
    }

    #[test]
    fn scope_is_asked_for_by_the_all_flag_alone() {
        assert_eq!(Scope::from_all_flag(true), Scope::Weave);
        assert_eq!(Scope::from_all_flag(false), Scope::ActiveProject);
        assert_eq!(Repair::from_fix_flag(true), Repair::Apply);
        assert_eq!(Repair::from_fix_flag(false), Repair::Report);
    }
}
