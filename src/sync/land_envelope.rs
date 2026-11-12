//! What the `rwv land` machine reported, and the `--json` envelope built
//! from that alone.
//!
//! A module of its own so that only [`LandRecorder`] writes an outcome's
//! fields: `sync.rs` is one module, and inside it privacy separates nothing.
//! On a resumed op the invocation and the machine name different workspaces,
//! and the envelope reports the machine's.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{
    in_manifest_order, ContainmentOutput, LandJsonOutput, LandingReportOutput, LeftBehindOutput,
    MemberOutput, OpCoordinates, OutputHandler, RepoSyncOutcome, RetiredWorkweave,
    Step3AdvanceOutput, SyncOutcomeOutput, LAND_JSON_SCHEMA_URL,
};
use crate::manifest::{RepoKey, WorkweaveName};
use crate::op_state::OpPhase;
use crate::path_spelling::WirePath;
use crate::placement::PlacementOutput;
use crate::workspace::Resolution;

/// Everything the machine reported through its handler during one run.
#[derive(Default)]
pub(super) struct LandOutcome {
    records: Vec<SyncOutcomeOutput>,
    step3_advances: HashMap<String, Step3AdvanceOutput>,
    project_repo_advance: Option<Step3AdvanceOutput>,
    coordinates: Option<OpCoordinates>,
    retired: Option<WorkweaveName>,
    survivors: Vec<crate::deletion::Survivor>,
    resumed: Option<OpPhase>,
    placements: Vec<PlacementOutput>,
    members: Vec<MemberOutput>,
    left_behind: Vec<LeftBehindOutput>,
    reports: Vec<LandingReportOutput>,
    complete: bool,
}

impl LandOutcome {
    pub(super) fn any_failure(&self) -> bool {
        self.records.iter().any(SyncOutcomeOutput::is_failure)
    }

    /// `None` when there is nothing to describe: the machine refused before it
    /// resolved its coordinates, or `failed` before recording an outcome,
    /// reporting a move or running its last phase, where the error is the
    /// whole story.
    pub(super) fn envelope(
        self,
        refused: Vec<crate::diagnosis::JudgedOutput>,
        failed: bool,
        resolution: Option<Resolution>,
    ) -> Option<LandJsonOutput> {
        if self.records.is_empty() && self.reports.is_empty() && failed && !self.complete {
            return None;
        }
        let coordinates = self.coordinates?;
        let mut step3_advances = self.step3_advances;
        // A resume that re-entered downstream of replay records no outcomes,
        // so an advance it performs has no record to ride on.
        let outcomes = in_manifest_order(self.records)
            .into_iter()
            .map(|mut outcome| {
                if let Some(advance) = step3_advances.remove(outcome.path()) {
                    *outcome.step3_advance_mut() = Some(advance);
                }
                outcome
            })
            .collect();
        Some(LandJsonOutput {
            schema: LAND_JSON_SCHEMA_URL.to_owned(),
            source_workweave: coordinates.source_workweave,
            target: coordinates.target.as_deref().map(WirePath::of),
            retired: self.retired.is_some(),
            survivors: self.survivors,
            resumed: self.resumed,
            outcomes,
            placements: self.placements,
            members: self.members,
            complete: self.complete,
            refused,
            project_repo_advance: self.project_repo_advance,
            left_behind: self.left_behind,
            reports: self.reports,
            resolution,
        })
    }
}

/// The envelope-mode handler for `rwv land --json`: it collects what the
/// machine reports into a [`LandOutcome`] and prints nothing.
#[derive(Default)]
pub(super) struct LandRecorder(Mutex<LandOutcome>);

impl LandRecorder {
    pub(super) fn into_outcome(self) -> LandOutcome {
        self.0.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    fn with(&self, report: impl FnOnce(&mut LandOutcome)) {
        report(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

impl OutputHandler for LandRecorder {
    fn emit_text(&self) -> bool {
        false
    }

    fn record(
        &self,
        path: &str,
        abs_path: &std::path::Path,
        outcome: &RepoSyncOutcome,
        containment: Option<ContainmentOutput>,
    ) {
        let out = SyncOutcomeOutput::from_outcome(
            path.to_owned(),
            WirePath::of(abs_path),
            outcome,
            containment,
        );
        self.with(|o| o.records.push(out));
    }

    fn record_step3_advance(&self, repo: &RepoKey, from_sha: &str, to_sha: &str) {
        let advance = Step3AdvanceOutput {
            from_sha: from_sha.to_owned(),
            to_sha: to_sha.to_owned(),
        };
        self.with(|o| match repo {
            RepoKey::Project => o.project_repo_advance = Some(advance),
            RepoKey::Member(path) => {
                o.step3_advances.insert(path.to_string(), advance);
            }
        });
    }

    fn record_placement(&self, placement: PlacementOutput) {
        self.with(|o| o.placements.push(placement));
    }

    fn record_member(&self, member: MemberOutput) {
        self.with(|o| o.members.push(member));
    }

    fn record_coordinates(&self, coordinates: OpCoordinates) {
        self.with(|o| o.coordinates = Some(coordinates));
    }

    fn record_retired(&self, retired: &RetiredWorkweave) {
        self.with(|o| {
            o.retired = Some(retired.name.clone());
            o.survivors = retired
                .deletion
                .rule
                .as_ref()
                .map(|rule| rule.survivors.clone())
                .unwrap_or_default();
        });
    }

    fn record_resumed(&self, phase: OpPhase) {
        self.with(|o| o.resumed = Some(phase));
    }

    fn record_complete(&self) {
        self.with(|o| o.complete = true);
    }

    fn record_left_behind(&self, left: LeftBehindOutput) {
        self.with(|o| o.left_behind.push(left));
    }

    fn record_landing_report(&self, report: LandingReportOutput) {
        self.with(|o| o.reports.push(report));
    }
}
