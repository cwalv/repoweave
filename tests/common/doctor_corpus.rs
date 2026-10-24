//! One sample of every finding `rwv doctor` can report, and the token that
//! names it, over both channels: `CheckViolation` for rwv's own scans and
//! `Issue` for what an integration raised.
//!
//! Shared because more than one instrument needs to walk the whole finding
//! space, and a second copy of the walk is a second thing to keep complete.
//! [`case_token`] and [`issue_kind_token`] match exhaustively, so a new
//! variant, sub-kind or issue kind stops every dependent test compiling until
//! a sample is added here.
//!
//! Held constant across every [`Issue`] sample: `severity` is always
//! `Warning`, `safe_to_fix` is always `true`, `integration` is always
//! `"go-work"`. The one [`CheckViolation::StaleOpState`] sample carries
//! `OpVerb::SyncTo` — no other verb is sampled here.
//! `tests/schema_conformance_test.rs`'s `doctor_corpus_supplement` produces
//! the values this corpus never does — `Severity::Error`, `safe_to_fix:
//! false`, `OpVerb::Sync` — for that file's schema conformance only; nothing
//! here inherits them.
//!
//! [`case_token`] is verb-free, so a second `StaleOpState` sample would
//! silently overwrite the first wherever a consumer keys on it —
//! `tests/doctor_fix_authority_test.rs`'s `register()` collects samples into
//! a `BTreeMap<String, _>`.

use repoweave::check::{
    BranchDisciplineKind, CheckViolation, CloneTopologyKind, DeadOpLeaseKind, DetachedAncestry,
    DriftKind, IndexDriftKind, LegacyRefAtTip, OrphanedSavepointKind, PermissionsKind,
    PlacementConvergence, ProvenanceKind, ReplayExclusionKind, SubmoduleCommitKind,
    WeaveRootIdentityConflictKind, WorkingTreeDriftKind, WorkweaveTreeIntegrityKind,
};
use repoweave::integration::{Issue, IssueKind, IssueSource, Severity};
use repoweave::integration::{MemberIncompatibility, Observed};
use repoweave::integrations::cargo_workspace::CargoSkewOccurrence;
use repoweave::integrations::GoWork;
use repoweave::manifest::{ProjectName, RepoPath, Role, WorkweaveId, WorkweaveName};
use repoweave::op_state::{OpVerb, SavepointKey};
use repoweave::vcs::{RawRevisionId, ResolvedRevisionId};
use repoweave::workspace::MarkerDefect;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The `--json` document a run holding exactly `violations` and `issues`
/// would print.
///
/// Assembled here from the per-record renderers and the shared envelope type,
/// because the report renderer takes only what the findings pipeline builds:
/// a test can hand it no corpus. The envelope is the one `rwv doctor --json`
/// serializes, so its bytes are still the wire's.
pub fn wire_document(
    violations: Vec<CheckViolation>,
    issues: Vec<Issue>,
    workspace_dir: &Path,
    workweave_dirs: &HashMap<WorkweaveId, PathBuf>,
    resolution: Option<repoweave::workspace::Resolution>,
    plugins: Vec<repoweave::plugins::PluginRecord>,
    advisories: Vec<repoweave::workspace::AdvisoryOutput>,
) -> repoweave::check::DoctorJsonOutput {
    use repoweave::check::{IssueOutput, ViolationOutput, DOCTOR_SCHEMA_URL};
    repoweave::check::DoctorJsonOutput {
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

pub fn project() -> ProjectName {
    ProjectName::new("proj").unwrap()
}

pub fn repo() -> RepoPath {
    RepoPath::new("github/acme/repo").unwrap()
}

pub fn workweave() -> WorkweaveId {
    WorkweaveId {
        project: project(),
        name: WorkweaveName::new("feat-a").unwrap(),
    }
}

pub fn sha() -> String {
    "a".repeat(40)
}

pub fn commit() -> repoweave::vcs::CommitId {
    repoweave::vcs::CommitId::parse(&sha()).unwrap()
}

pub fn rev() -> ResolvedRevisionId {
    ResolvedRevisionId::from_canonical(commit(), None)
}

pub fn path(p: &str) -> PathBuf {
    PathBuf::from(p)
}

/// The test's own identifier for one sample — not a wire value, though it is
/// spelled like one so a failure reads against the `--json` output.
///
/// The match is exhaustive on purpose: a new variant or sub-kind stops this
/// file compiling until whoever added it also adds a sample to [`corpus`].
pub fn case_token(v: &CheckViolation) -> String {
    match v {
        CheckViolation::OrphanedClone { .. } => "orphaned-clone".into(),
        CheckViolation::UnknownSegmentClone { .. } => "unknown-segment-clone".into(),
        CheckViolation::ConfusableSiblings { .. } => "confusable-siblings".into(),
        CheckViolation::DanglingReference { .. } => "dangling-reference".into(),
        CheckViolation::MemberPermissions { kind, .. } => match kind {
            PermissionsKind::WritableReference => "member-permissions/writable-reference".into(),
            PermissionsKind::ReadOnlyMember => "member-permissions/read-only-member".into(),
        },
        CheckViolation::RoleDisagreement { .. } => "role-disagreement".into(),
        CheckViolation::StaleLock { .. } => "stale-lock".into(),
        CheckViolation::IncompleteLock { .. } => "incomplete-lock".into(),
        CheckViolation::WorkweaveDrift { kind, .. } => match kind {
            DriftKind::Missing => "workweave-drift/missing".into(),
            DriftKind::MissingUnfetched => "workweave-drift/missing-unfetched".into(),
            DriftKind::Symlink => "workweave-drift/symlink".into(),
            DriftKind::SymlinkUnfetched => "workweave-drift/symlink-unfetched".into(),
            DriftKind::Extra => "workweave-drift/extra".into(),
            DriftKind::DanglingLink => "workweave-drift/dangling-link".into(),
            DriftKind::DanglingLinkUnfetched => "workweave-drift/dangling-link-unfetched".into(),
        },
        CheckViolation::IndexDrift { kind, .. } => match kind {
            IndexDriftKind::SafeToFix => "index-drift/safe-to-fix".into(),
            IndexDriftKind::LiveStaged => "index-drift/live-staged".into(),
        },
        CheckViolation::WorkingTreeDrift { kind, .. } => match kind {
            WorkingTreeDriftKind::SafeToFix => "working-tree-drift/safe-to-fix".into(),
            WorkingTreeDriftKind::LiveEdits => "working-tree-drift/live-edits".into(),
        },
        CheckViolation::MissingReplayExclusion { sub_kind, .. } => match sub_kind {
            ReplayExclusionKind::Absent => "missing-replay-exclusion/absent".into(),
            ReplayExclusionKind::LegacySpelling => {
                "missing-replay-exclusion/legacy-spelling".into()
            }
            ReplayExclusionKind::LegacyAlongsideCurrent => {
                "missing-replay-exclusion/legacy-alongside-current".into()
            }
        },
        CheckViolation::ReplayExclusionUnreadable { .. } => "replay-exclusion-unreadable".into(),
        CheckViolation::MissingMergeDriverConfig { .. } => "missing-merge-driver-config".into(),
        CheckViolation::MergeDriverConfigUnreadable { .. } => {
            "merge-driver-config-unreadable".into()
        }
        CheckViolation::HeadUnreadable { .. } => "head-unreadable".into(),
        CheckViolation::ProjectsDirUnreadable { .. } => "projects-dir-unreadable".into(),
        CheckViolation::ProjectlessDir { .. } => "projectless-dir".into(),
        CheckViolation::UnnameableProject { .. } => "unnameable-project".into(),
        CheckViolation::UnresolvableLockEntry { .. } => "unresolvable-lock-entry".into(),
        CheckViolation::LockPinnedByName { .. } => "lock-pinned-by-name".into(),
        CheckViolation::LegacyManifestFormat { .. } => "legacy-manifest-format".into(),
        CheckViolation::DanglingActiveProject { .. } => "dangling-active-project".into(),
        CheckViolation::UnreadableActiveProject { .. } => "unreadable-active-project".into(),
        CheckViolation::WeaveRootIdentityConflict { sub_kind, .. } => match sub_kind {
            WeaveRootIdentityConflictKind::RegisteredWorkweave { .. } => {
                "weave-root-identity-conflict/registered-workweave".into()
            }
            WeaveRootIdentityConflictKind::MarkerUnverifiable { .. } => {
                "weave-root-identity-conflict/marker-unverifiable".into()
            }
            WeaveRootIdentityConflictKind::Unwitnessed { .. } => {
                "weave-root-identity-conflict/unwitnessed".into()
            }
        },
        CheckViolation::LegacyWorkweaveMarker { .. } => "legacy-workweave-marker".into(),
        CheckViolation::LegacyWorkweaveIndex { .. } => "legacy-workweave-index".into(),
        CheckViolation::UnreadableOwnedState { .. } => "unreadable-owned-state".into(),
        CheckViolation::UnreadableWorkweaveIndex { .. } => "unreadable-workweave-index".into(),
        CheckViolation::UnparseableProject { .. } => "unparseable-project".into(),
        CheckViolation::WorkweaveTreeIntegrity { sub_kind, .. } => {
            let tail = match sub_kind {
                WorkweaveTreeIntegrityKind::DanglingParent { .. } => "dangling-parent",
                WorkweaveTreeIntegrityKind::ParentChainAnomaly { .. } => "parent-chain-anomaly",
                WorkweaveTreeIntegrityKind::UnregisteredDir => "unregistered-dir",
                WorkweaveTreeIntegrityKind::ForeignPrimary { .. } => "foreign-primary",
                WorkweaveTreeIntegrityKind::ForeignPrimaryOtherWorkspace { .. } => {
                    "foreign-primary-other-workspace"
                }
                WorkweaveTreeIntegrityKind::StaleRegistryEntry { .. } => "stale-registry-entry",
                WorkweaveTreeIntegrityKind::UnregisteredWorkweave { .. } => {
                    "unregistered-workweave"
                }
                WorkweaveTreeIntegrityKind::TrackedIndex { .. } => "tracked-index",
                WorkweaveTreeIntegrityKind::UnreadableMarker { .. } => "unreadable-marker",
                WorkweaveTreeIntegrityKind::MisnamedDir { .. } => "misnamed-dir",
                WorkweaveTreeIntegrityKind::NestedWorkweaveDir { .. } => "nested-workweave-dir",
            };
            format!("workweave-tree-integrity/{tail}")
        }
        CheckViolation::Provenance { sub_kind, .. } => {
            let tail = match sub_kind {
                ProvenanceKind::OriginUrlMismatch { .. } => "origin-url-mismatch",
                ProvenanceKind::RelativeRemote { .. } => "relative-remote",
                ProvenanceKind::UnresolvableUrl { .. } => "unresolvable-url",
                ProvenanceKind::LockShaUnreachable { .. } => "lock-sha-unreachable",
                ProvenanceKind::PlacementDisagreement { .. } => "placement-disagreement",
            };
            format!("provenance/{tail}")
        }
        CheckViolation::CloneTopology { sub_kind, .. } => {
            let tail = match sub_kind {
                CloneTopologyKind::StandaloneInWorkweave { .. } => "standalone-in-workweave",
                CloneTopologyKind::DisconnectedWeaveClone { .. } => "disconnected-weave-clone",
                CloneTopologyKind::WrongParentWorktree { .. } => "wrong-parent-worktree",
                CloneTopologyKind::WeaveCloneIsWorktree { .. } => "weave-clone-is-worktree",
            };
            format!("clone-topology/{tail}")
        }
        CheckViolation::BranchDiscipline { sub_kind, .. } => {
            let tail = match sub_kind {
                BranchDisciplineKind::SharedBranch { .. } => "shared-branch",
                BranchDisciplineKind::ForeignEphemeral { .. } => "foreign-ephemeral",
                BranchDisciplineKind::Detached { .. } => "detached",
                BranchDisciplineKind::BlockedEphemeralNamespace { .. } => {
                    "blocked-ephemeral-namespace"
                }
                BranchDisciplineKind::BlockedDetachedNamespace { .. } => {
                    "blocked-detached-namespace"
                }
                BranchDisciplineKind::DetachedOffEphemeralBranch { .. } => {
                    "detached-off-ephemeral-branch"
                }
                BranchDisciplineKind::UnmigratedEphemeralBranch { .. } => {
                    "unmigrated-ephemeral-branch"
                }
                BranchDisciplineKind::UnrecordedEphemeralBranch { .. } => {
                    "unrecorded-ephemeral-branch"
                }
                BranchDisciplineKind::UnbornCheckout { .. } => "unborn-checkout",
                BranchDisciplineKind::CanonicalHoldsLiveWorkweaveRef { .. } => {
                    "canonical-holds-live-workweave-ref"
                }
                BranchDisciplineKind::CanonicalHoldsLeakedRef { .. } => {
                    "canonical-holds-leaked-ref"
                }
                BranchDisciplineKind::CanonicalDetached { .. } => "canonical-detached",
                BranchDisciplineKind::StaleEphemeralBranchSafe { .. } => {
                    "stale-ephemeral-branch-safe"
                }
                BranchDisciplineKind::StaleEphemeralBranchLive { .. } => {
                    "stale-ephemeral-branch-live"
                }
                BranchDisciplineKind::StaleEphemeralBranchUnowned { .. } => {
                    "stale-ephemeral-branch-unowned"
                }
            };
            format!("branch-discipline/{tail}")
        }
        CheckViolation::StaleWorktreeRegistration { .. } => "stale-worktree-registration".into(),
        CheckViolation::StaleOpState { .. } => "stale-op-state".into(),
        CheckViolation::DeadOpLease { sub_kind, .. } => match sub_kind {
            DeadOpLeaseKind::OwnerRecordAbsent => "dead-op-lease/owner-record-absent".into(),
            DeadOpLeaseKind::OwnerOpIdMismatch { .. } => {
                "dead-op-lease/owner-op-id-mismatch".into()
            }
        },
        CheckViolation::DanglingRefReceipt { .. } => "dangling-ref-receipt".into(),
        CheckViolation::PreFlatRefReceipt { .. } => "pre-flat-ref-receipt".into(),
        CheckViolation::OrphanedSavepoint { sub_kind, .. } => match sub_kind {
            OrphanedSavepointKind::Redundant => "orphaned-savepoint/redundant".into(),
            OrphanedSavepointKind::Live => "orphaned-savepoint/live".into(),
        },
        CheckViolation::UnrecognizedSavepointRef { .. } => "unrecognized-savepoint-ref".into(),
        CheckViolation::CargoVersionSkew { .. } => "cargo-version-skew".into(),
        CheckViolation::CargoPatchShadowing { .. } => "cargo-patch-shadowing".into(),
        CheckViolation::MissingCanonicalClone { .. } => "missing-canonical-clone".into(),
        CheckViolation::UninitializedSubmodule { sub_kind, .. } => match sub_kind {
            SubmoduleCommitKind::Fetched => "uninitialized-submodule/fetched".into(),
            SubmoduleCommitKind::Unfetched => "uninitialized-submodule/unfetched".into(),
        },
        CheckViolation::PhantomMergeDriver { .. } => "phantom-merge-driver".into(),
    }
}

/// One sample per token [`case_token`] can produce.
///
/// Rebuilt on each call rather than cloned: `CheckViolation` is deliberately
/// not `Clone`, and both renderers consume what they are handed.
pub fn corpus() -> Vec<CheckViolation> {
    vec![
        CheckViolation::OrphanedClone { path: repo() },
        CheckViolation::UnknownSegmentClone { path: repo() },
        CheckViolation::DanglingReference {
            project: project(),
            repo: repo(),
            clone_at_placement: None,
        },
        CheckViolation::RoleDisagreement {
            repo: repo(),
            roles: vec![
                (project(), Role::Reference),
                (ProjectName::new("other").unwrap(), Role::Owned),
                (ProjectName::new("third").unwrap(), Role::Fork),
                (ProjectName::new("fourth").unwrap(), Role::Dependency),
            ],
        },
        CheckViolation::StaleLock {
            project: project(),
            repo: repo(),
            locked: rev(),
            actual: ResolvedRevisionId::from_canonical(
                repoweave::vcs::CommitId::parse(&"b".repeat(40)).unwrap(),
                None,
            ),
        },
        CheckViolation::IncompleteLock {
            project: project(),
            repo: repo(),
        },
        CheckViolation::MemberPermissions {
            workweave: Some(workweave()),
            repo: repo(),
            kind: PermissionsKind::WritableReference,
            files: 3,
        },
        CheckViolation::MemberPermissions {
            workweave: None,
            repo: repo(),
            kind: PermissionsKind::ReadOnlyMember,
            files: 1,
        },
        CheckViolation::WorkweaveDrift {
            workweave: Some(workweave()),
            kind: DriftKind::Missing,
            repo: repo(),
        },
        CheckViolation::WorkweaveDrift {
            workweave: Some(workweave()),
            kind: DriftKind::MissingUnfetched,
            repo: repo(),
        },
        CheckViolation::WorkweaveDrift {
            workweave: Some(workweave()),
            kind: DriftKind::Symlink,
            repo: repo(),
        },
        CheckViolation::WorkweaveDrift {
            workweave: Some(workweave()),
            kind: DriftKind::SymlinkUnfetched,
            repo: repo(),
        },
        CheckViolation::WorkweaveDrift {
            workweave: Some(workweave()),
            kind: DriftKind::Extra,
            repo: repo(),
        },
        CheckViolation::WorkweaveDrift {
            workweave: Some(workweave()),
            kind: DriftKind::DanglingLink,
            repo: repo(),
        },
        CheckViolation::WorkweaveDrift {
            workweave: Some(workweave()),
            kind: DriftKind::DanglingLinkUnfetched,
            repo: repo(),
        },
        CheckViolation::IndexDrift {
            workweave: Some(workweave()),
            repo: repo(),
            kind: IndexDriftKind::SafeToFix,
        },
        CheckViolation::IndexDrift {
            workweave: None,
            repo: repo(),
            kind: IndexDriftKind::LiveStaged,
        },
        CheckViolation::WorkingTreeDrift {
            workweave: Some(workweave()),
            repo: repo(),
            kind: WorkingTreeDriftKind::SafeToFix,
        },
        CheckViolation::WorkingTreeDrift {
            workweave: None,
            repo: repo(),
            kind: WorkingTreeDriftKind::LiveEdits,
        },
        CheckViolation::MissingReplayExclusion {
            project: project(),
            sub_kind: ReplayExclusionKind::Absent,
        },
        CheckViolation::MissingReplayExclusion {
            project: project(),
            sub_kind: ReplayExclusionKind::LegacySpelling,
        },
        CheckViolation::MissingReplayExclusion {
            project: project(),
            sub_kind: ReplayExclusionKind::LegacyAlongsideCurrent,
        },
        CheckViolation::ReplayExclusionUnreadable {
            project: project(),
            error: "permission denied".into(),
        },
        CheckViolation::MissingMergeDriverConfig {
            project: project(),
            config_key: "merge.rwv-ours.driver".into(),
        },
        CheckViolation::MergeDriverConfigUnreadable {
            project: project(),
            config_key: "merge.rwv-ours.driver".into(),
            error: "bad config line 3".into(),
        },
        CheckViolation::HeadUnreadable {
            repo: repo(),
            error: "not a git repository".into(),
        },
        CheckViolation::ProjectsDirUnreadable {
            path: path("/ws/projects"),
            error: "permission denied".into(),
        },
        CheckViolation::ProjectlessDir {
            dir: path("/ws/projects/scaffold"),
        },
        CheckViolation::UnnameableProject {
            dir: path("/ws/projects/bad--name"),
            derived: "bad--name".into(),
            error: "'bad--name' is not a valid project name: contains `--` or starts/ends \
                    with `-`, ambiguous against the `--` that joins project to workweave."
                .into(),
        },
        CheckViolation::UnresolvableLockEntry {
            project: project(),
            repo: repo(),
        },
        CheckViolation::LockPinnedByName {
            project: project(),
            repo: repo(),
            version: RawRevisionId::new("v1.2.0"),
        },
        CheckViolation::LegacyManifestFormat {
            project: project(),
            legacy_path: path("/ws/projects/proj/rwv.yaml"),
        },
        CheckViolation::DanglingActiveProject {
            project: project(),
            missing_dir: path("/ws/projects/proj"),
        },
        CheckViolation::UnreadableActiveProject {
            path: path("/ws/.rwv-active"),
            detail: "the file is empty".into(),
        },
        CheckViolation::WeaveRootIdentityConflict {
            root: path("/ws"),
            pointer_project: Some(project()),
            sub_kind: WeaveRootIdentityConflictKind::RegisteredWorkweave {
                project: project(),
                workweave_name: WorkweaveName::new("feat-a").unwrap(),
            },
        },
        CheckViolation::WeaveRootIdentityConflict {
            root: path("/ws"),
            pointer_project: Some(project()),
            sub_kind: WeaveRootIdentityConflictKind::MarkerUnverifiable {
                marker_path: path("/ws/.rwv-workweave"),
                defect: MarkerDefect::Legacy,
            },
        },
        CheckViolation::WeaveRootIdentityConflict {
            root: path("/ws"),
            pointer_project: None,
            sub_kind: WeaveRootIdentityConflictKind::Unwitnessed {
                detail: "no registry entry".into(),
            },
        },
        CheckViolation::LegacyWorkweaveMarker {
            marker_path: path("/ws/.rwv-workweave"),
            primary: path("/ws"),
        },
        CheckViolation::LegacyWorkweaveIndex {
            project: project(),
            index_path: path("/ws/projects/proj/.rwv-workweave-index"),
        },
        CheckViolation::UnreadableOwnedState {
            project: project(),
            state_path: path("/ws/projects/proj/.rwv-owned-digests"),
            error: "parse failed: expected value at line 1 column 1".into(),
            active: true,
        },
        CheckViolation::UnreadableWorkweaveIndex {
            project: project(),
            index_path: path("/ws/projects/proj/.rwv-workweave-index"),
            error: "expected value at line 1 column 1".into(),
        },
        CheckViolation::UnparseableProject {
            project: project(),
            manifest_path: path("/ws/projects/proj/rwv.toml"),
            message: "bad yaml".into(),
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::DanglingParent {
                parent_path: path("/gone"),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::ParentChainAnomaly {
                detail: "cycle".into(),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::UnregisteredDir,
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::ForeignPrimary {
                marker_primary: path("/other"),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::ForeignPrimaryOtherWorkspace {
                marker_primary: path("/other"),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::StaleRegistryEntry {
                project: project(),
                workweave_name: WorkweaveName::new("feat-a").unwrap(),
                recorded_path: path("/gone"),
                reason: "absent".into(),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::UnregisteredWorkweave {
                project: project(),
                workweave_name: WorkweaveName::new("feat-a").unwrap(),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::TrackedIndex {
                project: project(),
                index_path: path("/ws/projects/proj/.rwv-workweave-index"),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::UnreadableMarker {
                detail: "/ws/.workweaves/proj--feat-a/.rwv-workweave is a legacy (YAML) \
                         workweave marker with no `primary:` field, so it cannot be migrated \
                         automatically. Write it by hand as JSON with the three required \
                         fields: `primary`, `project`, and `parent`"
                    .into(),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/other--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::MisnamedDir {
                expected_dir_name: Some("proj--feat-a".into()),
                detail: "the marker records project `proj` and the name the registry \
                         records for this path is `feat-a`, so the records expect \
                         `proj--feat-a`"
                    .into(),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/chatly/web-app--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::NestedWorkweaveDir {
                project: ProjectName::new("chatly/web-app").unwrap(),
                workweave_name: WorkweaveName::new("feat-a").unwrap(),
                expected_dir_name: "chatly+web-app--feat-a".into(),
            },
        },
        CheckViolation::ConfusableSiblings {
            parent: "projects".into(),
            first: "Chatly".into(),
            second: "chatly".into(),
        },
        CheckViolation::Provenance {
            project: project(),
            repo: repo(),
            sub_kind: ProvenanceKind::OriginUrlMismatch {
                manifest_url: "https://example.invalid/a".into(),
                actual_url: "https://example.invalid/b".into(),
                is_reference_role: false,
            },
        },
        CheckViolation::Provenance {
            project: project(),
            repo: repo(),
            sub_kind: ProvenanceKind::RelativeRemote {
                path: "../../up.git".into(),
                remote: "origin".into(),
                manifest_remote: Some("https://example.invalid/a".into()),
            },
        },
        CheckViolation::Provenance {
            project: project(),
            repo: repo(),
            sub_kind: ProvenanceKind::UnresolvableUrl {
                manifest_url: "/srv/repos/a".into(),
            },
        },
        CheckViolation::Provenance {
            project: project(),
            repo: repo(),
            sub_kind: ProvenanceKind::LockShaUnreachable { sha: sha() },
        },
        CheckViolation::Provenance {
            project: project(),
            repo: repo(),
            sub_kind: ProvenanceKind::PlacementDisagreement {
                expected_path: "github/other/repo".into(),
                converge: PlacementConvergence::Here {
                    retire_first: vec![workweave()],
                },
            },
        },
        CheckViolation::CloneTopology {
            workspace_path: path("/ws"),
            repo: repo(),
            sub_kind: CloneTopologyKind::StandaloneInWorkweave {
                store_path: path("/ws/github/acme/repo"),
            },
        },
        CheckViolation::CloneTopology {
            workspace_path: path("/ws"),
            repo: repo(),
            sub_kind: CloneTopologyKind::DisconnectedWeaveClone {
                weave_store_path: path("/ws/github/acme/repo/.git"),
                other_store_path: path("/other/.git"),
            },
        },
        CheckViolation::CloneTopology {
            workspace_path: path("/ws"),
            repo: repo(),
            sub_kind: CloneTopologyKind::WrongParentWorktree {
                expected_store_path: path("/ws/github/acme/repo/.git"),
                actual_store_path: path("/other/.git"),
            },
        },
        CheckViolation::CloneTopology {
            workspace_path: path("/ws"),
            repo: repo(),
            sub_kind: CloneTopologyKind::WeaveCloneIsWorktree {
                actual_store_path: path("/other/.git"),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::SharedBranch {
                actual_branch: "main".into(),
                expected_ref: "proj--feat-a".into(),
                recorded_ref: None,
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::ForeignEphemeral {
                actual_branch: "proj--other".into(),
                expected_ref: "proj--feat-a".into(),
                recorded_ref: Some("proj--other".into()),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::Detached {
                expected_ref: "proj--feat-a".into(),
                recorded_ref: None,
                at_sha: commit(),
                legacy_branch: Some(LegacyRefAtTip {
                    branch: "proj--feat-a/main".into(),
                    tip_sha: Some(commit()),
                    strands_commits: false,
                }),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::UnmigratedEphemeralBranch {
                actual_branch: "proj--feat-a/main".into(),
                expected_ref: "proj--feat-a".into(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::BlockedEphemeralNamespace {
                expected_ref: "proj--feat-a".into(),
                blocking_refs: vec!["proj--feat-a/main".into(), "proj--feat-a/master".into()],
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::BlockedDetachedNamespace {
                expected_ref: "proj--feat-a".into(),
                at_sha: commit(),
                blocking_refs: vec!["proj--feat-a/main".into(), "proj--feat-a/master".into()],
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::DetachedOffEphemeralBranch {
                expected_ref: "proj--feat-a".into(),
                at_sha: commit(),
                branch_tip: commit(),
                ancestry: DetachedAncestry::HeadAhead,
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::UnrecordedEphemeralBranch {
                branch: "proj--feat-a".into(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::UnbornCheckout {
                branch: "proj--feat-a".into(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::CanonicalHoldsLiveWorkweaveRef {
                actual_branch: "proj--feat-a".into(),
                workweave: workweave(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::CanonicalHoldsLeakedRef {
                actual_branch: "proj--gone".into(),
                project: project(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::CanonicalDetached {
                at_sha: commit(),
                counterpart: Some("main".into()),
                reattachable: true,
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::StaleEphemeralBranchSafe {
                branch: "proj--gone".into(),
                project: project(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::StaleEphemeralBranchLive {
                branch: "proj--gone".into(),
                project: project(),
                tip_sha: commit(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::StaleEphemeralBranchUnowned {
                branch: "proj--gone".into(),
            },
        },
        CheckViolation::StaleWorktreeRegistration {
            workweave: Some(workweave()),
            repo: repo(),
            missing_path: path("/gone"),
        },
        CheckViolation::StaleOpState {
            workspace_dir: path("/ws"),
            verb: OpVerb::SyncTo,
            started_at: "2026-01-01T00:00:00Z".into(),
        },
        CheckViolation::DeadOpLease {
            workspace_dir: path("/ws"),
            op_id: "op-1".into(),
            recorded_owner: path("/ws"),
            sub_kind: DeadOpLeaseKind::OwnerRecordAbsent,
            created_at: None,
        },
        CheckViolation::DeadOpLease {
            workspace_dir: path("/ws"),
            op_id: "op-1".into(),
            recorded_owner: path("/ws"),
            sub_kind: DeadOpLeaseKind::OwnerOpIdMismatch {
                owner_op_id: "op-2".into(),
            },
            created_at: Some("2026-01-01T00:00:00Z".into()),
        },
        CheckViolation::DanglingRefReceipt {
            project: project(),
            store_path: path("/ws/github/acme/repo"),
            ref_name: "proj--feat-a".into(),
        },
        CheckViolation::PreFlatRefReceipt {
            project: project(),
            store_path: path("/ws/github/acme/repo"),
            ref_name: "proj--feat-a/main".into(),
        },
        CheckViolation::OrphanedSavepoint {
            workweave: Some(workweave()),
            repo: repo(),
            key: SavepointKey::Op("op-1".parse().unwrap()),
            sub_kind: OrphanedSavepointKind::Redundant,
        },
        CheckViolation::OrphanedSavepoint {
            workweave: None,
            repo: repo(),
            key: SavepointKey::Op("op-1".parse().unwrap()),
            sub_kind: OrphanedSavepointKind::Live,
        },
        CheckViolation::UnrecognizedSavepointRef {
            workweave: Some(workweave()),
            repo: repo(),
            holder: path("/ws/.workweaves/p--w/github/acme/repo"),
            ref_name: "refs/rwv/pre-op/not.a.key".into(),
        },
        CheckViolation::CargoVersionSkew {
            crate_name: "serde".into(),
            occurrences: vec![CargoSkewOccurrence {
                member: "github/acme/repo".into(),
                requirement: "1.0".into(),
            }],
        },
        CheckViolation::CargoPatchShadowing {
            weave_config: path("/ws/Cargo.toml"),
            member_config: path("/ws/github/acme/repo/Cargo.toml"),
            registry: "crates-io".into(),
            crate_name: "serde".into(),
        },
        CheckViolation::MissingCanonicalClone {
            workweave: workweave(),
            repo: repo(),
            canonical_path: path("/ws/github/acme/repo"),
        },
        CheckViolation::UninitializedSubmodule {
            workweave: workweave(),
            repo: repo(),
            sub_kind: SubmoduleCommitKind::Fetched,
            empty_paths: vec!["vendor/dep".into()],
        },
        CheckViolation::UninitializedSubmodule {
            workweave: workweave(),
            repo: repo(),
            sub_kind: SubmoduleCommitKind::Unfetched,
            empty_paths: vec!["vendor/dep".into()],
        },
        CheckViolation::PhantomMergeDriver {
            repo: repo(),
            pattern: "rwv.lock".into(),
            driver: "rwv-nope".into(),
        },
    ]
}

// ---------------------------------------------------------------------------
// The other finding channel: `Issue`
// ---------------------------------------------------------------------------

/// One sample of every [`IssueKind`], the discriminant integration findings
/// travel under on `rwv doctor --json`'s `issues` array.
///
/// Same construction as [`corpus`] and for the same reason: a new kind stops
/// [`issue_kind_token`] compiling until a sample is added here.
pub fn issue_corpus() -> Vec<Issue> {
    [
        IssueKind::ToolMissing,
        IssueKind::ManagedFileMissing,
        IssueKind::ManagedFileDrift,
        IssueKind::ManagedFileUserHeld,
        IssueKind::Surfacing,
        IssueKind::ConfigRejected,
        IssueKind::MalformedSettings,
        IssueKind::MemberIncompatibility(Box::new(MemberIncompatibility::new(
            &GoWork,
            &path("/ws/projects/proj/go.work"),
            Observed {
                key: "go",
                on_disk: "1.21",
                required: "1.23",
                required_by: "github/acme/repo/go.mod",
            },
        ))),
        IssueKind::DerivedStateStale,
        IssueKind::DisabledIntegrationArtifact,
        IssueKind::IntegrationFailed,
        IssueKind::CoreFinding,
    ]
    .into_iter()
    .map(|kind| Issue {
        source: IssueSource::integration("go-work"),
        severity: Severity::Warning,
        message: format!("sample finding for `{}`", kind.tag()).into(),
        kind,
        safe_to_fix: true,
    })
    .collect()
}

/// The wire tag one sample must arrive under, taken from [`IssueKind::tag`]
/// rather than retyped — the published tag and the token this compares it
/// against would otherwise be two spellings of one value.
///
/// The match is exhaustive for the same reason [`case_token`]'s is.
pub fn issue_kind_token(kind: &IssueKind) -> String {
    match kind {
        IssueKind::ToolMissing
        | IssueKind::ManagedFileMissing
        | IssueKind::ManagedFileDrift
        | IssueKind::ManagedFileUserHeld
        | IssueKind::Surfacing
        | IssueKind::ConfigRejected
        | IssueKind::MalformedSettings
        | IssueKind::MemberIncompatibility(_)
        | IssueKind::DerivedStateStale
        | IssueKind::DisabledIntegrationArtifact
        | IssueKind::IntegrationFailed
        | IssueKind::CoreFinding => kind.tag().to_string(),
    }
}
