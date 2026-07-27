//! `rwv doctor` renders one collected violation vector two ways — a text
//! report and the `--json` wire format — and this file pins the two against
//! each other in both directions.
//!
//! The regression it exists for: `scan_pre_flat_receipts` had exactly two call
//! sites, the text path and its own `--fix` helper. The JSON path called its
//! own collection function, which never named the scan, so
//! `rwv doctor --json` emitted zero `pre-flat-ref-receipt` findings while the
//! text report listed them. The convention that text is a subset of JSON held
//! in reverse and nothing noticed, because the convention was prose.
//!
//! Two instruments, because there are two ways to lose a finding:
//!
//!   1. **A renderer drops a kind it was handed.** [`corpus`] holds one sample
//!      per `(kind, sub-kind)` pair the enum can take; both renderers must
//!      produce output for each, save an explicit by-design list.
//!   2. **A collector is never called on one path.** The reachability walk
//!      below re-derives the call graph from the source and requires every
//!      violation-producing scan to sit under the one collection pipeline.
//!
//! Each instrument ships a seeded failure that plants the defect and asserts
//! the check reports it, and a non-vacuity assertion so a parser that silently
//! reads nothing cannot pass as clean.
//!
//! **Residue** — what these do *not* cover:
//!
//!   - Findings with no `CheckViolation` variant at all. Unreadable HEADs,
//!     unresolvable lock entries, the legacy `merge=ours` replay-exclusion
//!     spelling and the missing `merge.rwv-ours.driver` config reach the text
//!     report as bare `Issue`s and are absent from `--json` by construction.
//!     Nothing here sees them.
//!   - Integration-runner, `verify()` and surfacing findings, for the same
//!     reason.
//!   - A collector whose name does not start with `scan_` (and is not
//!     `find_violations`) is invisible to the reachability walk.
//!   - Sub-kind coverage is only as complete as [`corpus`]. `case_token`
//!     matches exhaustively, so a new variant or sub-kind fails to compile
//!     until a sample is added — but a sub-kind carrying a *field* whose value
//!     changes a renderer's mind is sampled once, at one value.

mod common;

use repoweave::check::{
    build_doctor_json, violations_to_issues, BranchDisciplineKind, CheckViolation,
    CloneTopologyKind, DeadOpLeaseKind, DriftKind, IndexDriftKind, LegacyRefAtTip,
    OrphanedSavepointKind, ProvenanceKind, WeaveRootIdentityConflictKind, WorkingTreeDriftKind,
    WorkweaveTreeIntegrityKind,
};
use repoweave::integrations::cargo_workspace::CargoSkewOccurrence;
use repoweave::manifest::{ProjectName, RepoPath, WorkweaveName};
use repoweave::vcs::ResolvedRevisionId;
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

fn project() -> ProjectName {
    ProjectName::new("proj").unwrap()
}

fn repo() -> RepoPath {
    RepoPath::new("github/acme/repo").unwrap()
}

fn workweave() -> WorkweaveName {
    WorkweaveName::new("feat-a").unwrap()
}

fn sha() -> String {
    "a".repeat(40)
}

fn rev() -> ResolvedRevisionId {
    ResolvedRevisionId::from_canonical(sha(), None)
}

fn path(p: &str) -> PathBuf {
    PathBuf::from(p)
}

/// The test's own identifier for one sample — not a wire value, though it is
/// spelled like one so a failure reads against the `--json` output.
///
/// The match is exhaustive on purpose: a new variant or sub-kind stops this
/// file compiling until whoever added it also adds a sample to [`corpus`].
fn case_token(v: &CheckViolation) -> String {
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
        CheckViolation::MissingReplayExclusion { .. } => "missing-replay-exclusion".into(),
        CheckViolation::LegacyRolePrimary { .. } => "legacy-role-primary".into(),
        CheckViolation::DanglingActiveProject { .. } => "dangling-active-project".into(),
        CheckViolation::WeaveRootIdentityConflict { sub_kind, .. } => match sub_kind {
            WeaveRootIdentityConflictKind::RegisteredWorkweave { .. } => {
                "weave-root-identity-conflict/registered-workweave".into()
            }
            WeaveRootIdentityConflictKind::Unwitnessed { .. } => {
                "weave-root-identity-conflict/unwitnessed".into()
            }
        },
        CheckViolation::LegacyWorkweaveMarker { .. } => "legacy-workweave-marker".into(),
        CheckViolation::LegacyWorkweaveIndex { .. } => "legacy-workweave-index".into(),
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
fn corpus() -> Vec<CheckViolation> {
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
        CheckViolation::MissingReplayExclusion { project: project() },
        CheckViolation::LegacyRolePrimary {
            project: project(),
            manifest_path: path("/ws/projects/proj/rwv.yaml"),
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
        CheckViolation::UnparseableProject {
            project: project(),
            manifest_path: path("/ws/projects/proj/rwv.yaml"),
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
// Instrument 1: neither renderer may drop a kind
// ---------------------------------------------------------------------------

/// The one kind the text report suppresses on purpose.
///
/// A foreign-primary marker resolving to a *different* valid workspace is
/// expected under a shared workweave container and is not this workspace's
/// problem — every sibling weave's doctor would otherwise repeat the same
/// finding about every other sibling. `--json` still carries it, because a
/// consumer correlating across weaves is exactly who wants it.
const JSON_ONLY_BY_DESIGN: &[&str] = &["workweave-tree-integrity/foreign-primary-other-workspace"];

fn renders_as_text(v: CheckViolation) -> bool {
    !violations_to_issues(vec![v]).is_empty()
}

fn renders_as_json(v: CheckViolation) -> bool {
    let mut workweave_dirs = HashMap::new();
    workweave_dirs.insert(workweave(), path("/ws/.workweaves/proj--feat-a"));
    // Serialized rather than field-accessed: parity is a claim about the two
    // rendered surfaces, so this side must read the emitted JSON.
    let doc = serde_json::to_value(build_doctor_json(
        vec![v],
        &path("/ws"),
        &workweave_dirs,
        None,
        Vec::new(),
    ))
    .expect("doctor payload serializes");
    doc["violations"]
        .as_array()
        .and_then(|vs| vs.first())
        .and_then(|v| v["kind"].as_str())
        .is_some()
}

/// Compare the two renderers over [`corpus`] and return one line per
/// disagreement.
///
/// The renderers come in as parameters so a seeded failure can hand this a
/// deliberately holed one and require the disagreement to be reported.
fn parity_gaps(
    text: &dyn Fn(CheckViolation) -> bool,
    json: &dyn Fn(CheckViolation) -> bool,
    json_only_by_design: &[&str],
) -> Vec<String> {
    let tokens: Vec<String> = corpus().iter().map(case_token).collect();
    let text_seen: Vec<bool> = corpus().into_iter().map(text).collect();
    let json_seen: Vec<bool> = corpus().into_iter().map(json).collect();

    let mut gaps = Vec::new();
    for ((token, in_text), in_json) in tokens.iter().zip(text_seen).zip(json_seen) {
        let excused = json_only_by_design.contains(&token.as_str());
        match (in_text, in_json) {
            (true, false) => gaps.push(format!(
                "{token}: the text report renders it, --json does not"
            )),
            (false, true) if !excused => gaps.push(format!(
                "{token}: --json renders it, the text report does not"
            )),
            (false, false) => gaps.push(format!("{token}: neither renderer produced output")),
            (true, true) if excused => gaps.push(format!(
                "{token}: listed as JSON-only by design, but the text report renders it — \
                 drop it from the list"
            )),
            _ => {}
        }
    }
    gaps
}

#[test]
fn every_violation_kind_reaches_both_renderers() {
    let gaps = parity_gaps(&renders_as_text, &renders_as_json, JSON_ONLY_BY_DESIGN);
    assert!(
        gaps.is_empty(),
        "text and --json disagree about which findings exist:\n  {}",
        gaps.join("\n  ")
    );
}

#[test]
fn the_parity_check_reports_a_text_renderer_that_drops_a_kind() {
    // Seeded with the suppression the code really carries, by withdrawing the
    // excuse for it. A check that reads nothing reports nothing here.
    let gaps = parity_gaps(&renders_as_text, &renders_as_json, &[]);
    assert_eq!(
        gaps,
        vec![
            "workweave-tree-integrity/foreign-primary-other-workspace: --json renders it, \
             the text report does not"
                .to_string()
        ],
        "withdrawing the by-design excuse must surface exactly the suppressed kind"
    );
}

#[test]
fn the_parity_check_reports_a_json_renderer_that_drops_a_kind() {
    // The reported regression, planted: a collector reachable from the text
    // path and not the JSON one shows up here as a kind --json cannot produce.
    let holed = |v: CheckViolation| {
        if matches!(v, CheckViolation::PreFlatRefReceipt { .. }) {
            return false;
        }
        renders_as_json(v)
    };
    let gaps = parity_gaps(&renders_as_text, &holed, JSON_ONLY_BY_DESIGN);
    assert!(
        gaps.contains(
            &"pre-flat-ref-receipt: the text report renders it, --json does not".to_string()
        ),
        "a --json renderer that drops a kind must be reported; got:\n  {}",
        gaps.join("\n  ")
    );
}

#[test]
fn the_corpus_covers_every_kind_and_is_not_empty() {
    let tokens: Vec<String> = corpus().iter().map(case_token).collect();
    assert!(
        tokens.len() >= 50,
        "the corpus walk yielded only {} samples — a parser or builder that \
         reads nothing passes every comparison above",
        tokens.len()
    );
    let unique: std::collections::BTreeSet<&String> = tokens.iter().collect();
    assert_eq!(
        unique.len(),
        tokens.len(),
        "duplicate sample tokens hide a missing case: {tokens:?}"
    );
    for expected in [
        "pre-flat-ref-receipt",
        "dangling-ref-receipt",
        "index-drift/safe-to-fix",
        "workweave-tree-integrity/foreign-primary-other-workspace",
    ] {
        assert!(
            unique.iter().any(|t| t.as_str() == expected),
            "the corpus must carry a `{expected}` sample"
        );
    }
}

// ---------------------------------------------------------------------------
// Instrument 2: every collector sits under the one collection pipeline
// ---------------------------------------------------------------------------

/// The function the whole doctor collects through.
const PIPELINE: &str = "collect_doctor_violations";

/// Top-level function bodies in a Rust source file, keyed by name.
///
/// Crude on purpose: a top-level `fn` starts at column zero and ends at the
/// first line that is exactly `}`. That is the shape `src/check.rs` is
/// written in, and a parser that understood more would be a second thing to
/// keep correct.
fn top_level_fns(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in source.lines() {
        if let Some((name, body)) = current.as_mut() {
            if line == "}" {
                out.push((name.clone(), body.join("\n")));
                current = None;
            } else {
                body.push(line);
            }
            continue;
        }
        let decl = line
            .strip_prefix("pub(crate) fn ")
            .or_else(|| line.strip_prefix("pub fn "))
            .or_else(|| line.strip_prefix("fn "));
        if let Some(rest) = decl {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                current = Some((name, vec![line]));
            }
        }
    }
    out
}

/// The declaration of a function body: everything up to and including the
/// first line carrying the opening brace.
fn signature_of(body: &str) -> String {
    let mut sig = Vec::new();
    for line in body.lines() {
        sig.push(line);
        if line.contains('{') {
            break;
        }
    }
    sig.join("\n")
}

/// Names of the functions that produce `CheckViolation`s directly — those
/// taking one out by `&mut Vec` or handing one back.
fn collectors(fns: &[(String, String)]) -> Vec<String> {
    fns.iter()
        .filter(|(name, body)| {
            (name.starts_with("scan_") || name == "find_violations")
                && signature_of(body).contains("CheckViolation")
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Every function name reachable from `root` by name occurrence.
fn reachable_from(fns: &[(String, String)], root: &str) -> std::collections::BTreeSet<String> {
    let bodies: HashMap<&str, &str> = fns.iter().map(|(n, b)| (n.as_str(), b.as_str())).collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = vec![root.to_string()];
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(body) = bodies.get(name.as_str()) else {
            continue;
        };
        for (candidate, _) in fns {
            if !seen.contains(candidate) && body.contains(&format!("{candidate}(")) {
                queue.push(candidate.clone());
            }
        }
    }
    seen
}

/// Collectors the pipeline cannot reach — each one a finding kind that exists
/// on some other path and in no report.
fn unreachable_collectors(source: &str, pipeline: &str) -> Vec<String> {
    let fns = top_level_fns(source);
    let reached = reachable_from(&fns, pipeline);
    collectors(&fns)
        .into_iter()
        .filter(|c| !reached.contains(c))
        .collect()
}

fn check_source() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/check.rs"))
        .expect("src/check.rs is readable from the test binary")
}

#[test]
fn every_violation_scan_is_reachable_from_the_collection_pipeline() {
    let orphans = unreachable_collectors(&check_source(), PIPELINE);
    assert!(
        orphans.is_empty(),
        "these scans produce findings no report can reach — call them from \
         `{PIPELINE}` rather than from a renderer or a `--fix` helper:\n  {}",
        orphans.join("\n  ")
    );
}

#[test]
fn the_reachability_check_reports_a_scan_the_pipeline_never_calls() {
    // Two plants, because the defect has two shapes: a scan only the text
    // renderer calls, and a scan only its own `--fix` helper calls. The second
    // is the one that actually shipped.
    let seeded = "\
fn scan_only_the_text_path_calls(out: &mut Vec<CheckViolation>) {
}
fn scan_only_the_fixer_calls(out: &mut Vec<CheckViolation>) {
}
fn scan_the_pipeline_calls(out: &mut Vec<CheckViolation>) {
}
fn fix_something() {
    scan_only_the_fixer_calls(&mut v);
}
fn run_check() {
    scan_only_the_text_path_calls(&mut v);
}
fn collect_doctor_violations() {
    scan_the_pipeline_calls(&mut v);
}
";
    let mut orphans = unreachable_collectors(seeded, PIPELINE);
    orphans.sort();
    assert_eq!(
        orphans,
        vec![
            "scan_only_the_fixer_calls".to_string(),
            "scan_only_the_text_path_calls".to_string(),
        ],
        "both shapes of an unreachable scan must be reported, and the scan the \
         pipeline does call must not be"
    );
}

#[test]
fn the_reachability_walk_actually_reads_the_source() {
    let source = check_source();
    let fns = top_level_fns(&source);
    assert!(
        fns.len() >= 60,
        "the function parser found only {} top-level fns in src/check.rs — it \
         has stopped reading the file, and every reachability assertion above \
         is vacuous",
        fns.len()
    );
    for expected in [
        "run_check",
        "run_check_json",
        PIPELINE,
        "find_violations",
        "violations_to_issues",
    ] {
        assert!(
            fns.iter().any(|(name, _)| name == expected),
            "the parser did not recover `{expected}`, so it is not reading the \
             shapes this check reasons about"
        );
    }
    let found = collectors(&fns);
    assert!(
        found.len() >= 10,
        "only {} violation-producing scans found: {found:?}",
        found.len()
    );
    assert!(
        found.iter().any(|c| c == "scan_pre_flat_receipts"),
        "the scan this file exists for must be among the collectors: {found:?}"
    );
    let reached = reachable_from(&fns, PIPELINE);
    assert!(
        reached.len() >= 10,
        "the call-graph walk from `{PIPELINE}` visited only {} functions",
        reached.len()
    );
}

// ---------------------------------------------------------------------------
// Instrument 3: the reported bug, end to end
// ---------------------------------------------------------------------------

/// A receipt whose recorded name carries a `/` segment, in a workspace where
/// no workweave mints that name. Both renderers must report it.
///
/// This is the fixture the regression was found on. The two instruments above
/// reason about renderers and about the source; this one runs the binary.
#[test]
fn a_pre_flat_receipt_reaches_both_the_report_and_the_wire_format() {
    use repoweave::git::git_vcs;
    use repoweave::vcs::{EphemeralRefName, LegacyEphemeralRefName, RawRefName};
    use repoweave::workweave_index::RefRegistry;

    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects").join("myproj")).unwrap();

    let canonical = ws.join("github").join("acme").join("repo");
    std::fs::create_dir_all(&canonical).unwrap();
    let git_in = |args: &[&str]| {
        let out = common::git()
            .args(args)
            .current_dir(&canonical)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git_in(&["init", "--initial-branch=main", "-q"]);
    git_in(&["config", "user.email", "test@test"]);
    git_in(&["config", "user.name", "Test"]);
    std::fs::write(canonical.join("README.md"), "init\n").unwrap();
    git_in(&["add", "README.md"]);
    git_in(&["commit", "-q", "-m", "init"]);

    // No workweave `ghost` on disk, so nothing mints `myproj--ghost/main`.
    git_in(&["branch", "myproj--ghost/main", "main"]);

    let proj = ProjectName::new("myproj").unwrap();
    let flat = EphemeralRefName::mint(&proj, &WorkweaveName::new("ghost").unwrap());
    let observed = RawRefName::new("myproj--ghost/main");
    let legacy = LegacyEphemeralRefName::claim(&flat, &observed).unwrap();
    let tip = git_vcs()
        .resolve_local_branch_tip(&canonical, &observed)
        .unwrap()
        .expect("the pre-flat branch exists");
    RefRegistry::for_project(&ws, &proj)
        .adopt_legacy(&canonical, legacy, tip)
        .expect("receipt recorded");

    let text = common::rwv()
        .args(["doctor", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let text_stdout = String::from_utf8_lossy(&text.stdout).into_owned();
    assert!(
        text_stdout.contains("myproj--ghost/main") && text_stdout.contains("carries a `/` segment"),
        "the text report must carry the finding; got:\n{text_stdout}"
    );

    let json = common::rwv()
        .args(["doctor", "--json", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let json_stdout = String::from_utf8_lossy(&json.stdout).into_owned();
    let doc: serde_json::Value = serde_json::from_str(&json_stdout)
        .unwrap_or_else(|e| panic!("`doctor --json` did not emit JSON ({e}):\n{json_stdout}"));
    let kinds: Vec<&str> = doc["violations"]
        .as_array()
        .expect("the document carries a violations array")
        .iter()
        .filter_map(|v| v["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"pre-flat-ref-receipt"),
        "`doctor --json` must emit the finding the text report shows; kinds were \
         {kinds:?}, document:\n{json_stdout}"
    );

    let recorded = doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "pre-flat-ref-receipt")
        .expect("the finding is in the document");
    assert_eq!(
        recorded["ref_name"], "myproj--ghost/main",
        "and it must name the receipt, not just the kind: {recorded}"
    );
}
