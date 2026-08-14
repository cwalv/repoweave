//! One sample of every finding `rwv doctor` can report, and the token that
//! names it, over both channels: `CheckViolation` for rwv's own scans and
//! `Issue` for what an integration raised.
//!
//! Shared because more than one instrument needs to walk the whole finding
//! space, and a second copy of the walk is a second thing to keep complete.
//! [`case_token`] and [`issue_kind_token`] match exhaustively, so a new
//! variant, sub-kind or issue kind stops every dependent test compiling until
//! a sample is added here.

use repoweave::check::{
    BranchDisciplineKind, CheckViolation, CloneTopologyKind, DeadOpLeaseKind, DriftKind,
    IndexDriftKind, LegacyRefAtTip, OrphanedSavepointKind, ProvenanceKind, ReplayExclusionKind,
    WeaveRootIdentityConflictKind, WorkingTreeDriftKind, WorkweaveTreeIntegrityKind,
};
use repoweave::integration::{Issue, IssueKind, Severity};
use repoweave::integrations::cargo_workspace::CargoSkewOccurrence;
use repoweave::integrations::merge::MemberIncompatibility;
use repoweave::manifest::{ProjectName, RepoPath, WorkweaveName};
use repoweave::op_state::OpVerb;
use repoweave::vcs::ResolvedRevisionId;
use repoweave::workspace::MarkerDefect;
use std::path::PathBuf;

pub fn project() -> ProjectName {
    ProjectName::new("proj").unwrap()
}

pub fn repo() -> RepoPath {
    RepoPath::new("github/acme/repo").unwrap()
}

pub fn workweave() -> WorkweaveName {
    WorkweaveName::new("feat-a").unwrap()
}

pub fn sha() -> String {
    "a".repeat(40)
}

pub fn rev() -> ResolvedRevisionId {
    ResolvedRevisionId::from_canonical(sha(), None)
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
        CheckViolation::DanglingReference { .. } => "dangling-reference".into(),
        CheckViolation::MissingRole { .. } => "missing-role".into(),
        CheckViolation::StaleLock { .. } => "stale-lock".into(),
        CheckViolation::IncompleteLock { .. } => "incomplete-lock".into(),
        CheckViolation::WorkweaveDrift { kind, .. } => match kind {
            DriftKind::Missing => "workweave-drift/missing".into(),
            DriftKind::Extra => "workweave-drift/extra".into(),
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
        CheckViolation::UnresolvableLockEntry { .. } => "unresolvable-lock-entry".into(),
        CheckViolation::LegacyManifestFormat { .. } => "legacy-manifest-format".into(),
        CheckViolation::DanglingActiveProject { .. } => "dangling-active-project".into(),
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
            };
            format!("workweave-tree-integrity/{tail}")
        }
        CheckViolation::Provenance { sub_kind, .. } => {
            let tail = match sub_kind {
                ProvenanceKind::OriginUrlMismatch { .. } => "origin-url-mismatch",
                ProvenanceKind::LockShaUnreachable { .. } => "lock-sha-unreachable",
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
        CheckViolation::CargoVersionSkew { .. } => "cargo-version-skew".into(),
        CheckViolation::CargoPatchShadowing { .. } => "cargo-patch-shadowing".into(),
        CheckViolation::MissingCanonicalClone { .. } => "missing-canonical-clone".into(),
        CheckViolation::UninitializedSubmodule { .. } => "uninitialized-submodule".into(),
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
        CheckViolation::DanglingReference {
            project: project(),
            repo: repo(),
        },
        CheckViolation::MissingRole {
            project: project(),
            repo: repo(),
        },
        CheckViolation::StaleLock {
            project: project(),
            repo: repo(),
            locked: rev(),
            actual: ResolvedRevisionId::from_canonical("b".repeat(40), None),
        },
        CheckViolation::IncompleteLock {
            project: project(),
            repo: repo(),
        },
        CheckViolation::WorkweaveDrift {
            workweave: workweave(),
            kind: DriftKind::Missing,
            repo: repo(),
        },
        CheckViolation::WorkweaveDrift {
            workweave: workweave(),
            kind: DriftKind::Extra,
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
        CheckViolation::UnresolvableLockEntry {
            project: project(),
            repo: repo(),
        },
        CheckViolation::LegacyManifestFormat {
            project: project(),
            legacy_path: path("/ws/projects/proj/rwv.yaml"),
        },
        CheckViolation::DanglingActiveProject {
            project: project(),
            missing_dir: path("/ws/projects/proj"),
        },
        CheckViolation::WeaveRootIdentityConflict {
            root: path("/ws"),
            pointer_project: Some(project()),
            sub_kind: WeaveRootIdentityConflictKind::RegisteredWorkweave {
                project: "proj".into(),
                workweave_name: "feat-a".into(),
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
                project: "proj".into(),
                workweave_name: "feat-a".into(),
                recorded_path: path("/gone"),
                reason: "absent".into(),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::UnregisteredWorkweave {
                project: "proj".into(),
                workweave_name: "feat-a".into(),
            },
        },
        CheckViolation::WorkweaveTreeIntegrity {
            workweave_dir: path("/ws/.workweaves/proj--feat-a"),
            sub_kind: WorkweaveTreeIntegrityKind::TrackedIndex {
                project: "proj".into(),
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
            sub_kind: ProvenanceKind::LockShaUnreachable { sha: sha() },
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
                at_sha: sha(),
                legacy_branch: Some(LegacyRefAtTip {
                    branch: "proj--feat-a/main".into(),
                    tip_sha: sha(),
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
                at_sha: sha(),
                blocking_refs: vec!["proj--feat-a/main".into(), "proj--feat-a/master".into()],
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
                workweave_name: "feat-a".into(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::CanonicalHoldsLeakedRef {
                actual_branch: "proj--gone".into(),
                project: "proj".into(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::CanonicalDetached {
                at_sha: sha(),
                counterpart: Some("main".into()),
                reattachable: true,
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::StaleEphemeralBranchSafe {
                branch: "proj--gone".into(),
                project: "proj".into(),
            },
        },
        CheckViolation::BranchDiscipline {
            repo_path: path("/ws/github/acme/repo"),
            sub_kind: BranchDisciplineKind::StaleEphemeralBranchLive {
                branch: "proj--gone".into(),
                project: "proj".into(),
                tip_sha: sha(),
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
            op_id: "op-1".into(),
            sub_kind: OrphanedSavepointKind::Redundant,
        },
        CheckViolation::OrphanedSavepoint {
            workweave: None,
            repo: repo(),
            op_id: "op-1".into(),
            sub_kind: OrphanedSavepointKind::Live,
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
        IssueKind::MemberIncompatibility(Box::new(MemberIncompatibility::new(
            "go-work",
            &path("/ws/projects/proj/go.work"),
            "go",
            "1.21",
            "1.23",
            "github/acme/repo/go.mod",
        ))),
        IssueKind::DerivedStateStale,
        IssueKind::DisabledIntegrationArtifact,
        IssueKind::IntegrationFailed,
        IssueKind::CoreFinding,
    ]
    .into_iter()
    .map(|kind| Issue {
        integration: "go-work".into(),
        severity: Severity::Warning,
        message: format!("sample finding for `{}`", kind.tag()),
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
        | IssueKind::MemberIncompatibility(_)
        | IssueKind::DerivedStateStale
        | IssueKind::DisabledIntegrationArtifact
        | IssueKind::IntegrationFailed
        | IssueKind::CoreFinding => kind.tag().to_string(),
    }
}
