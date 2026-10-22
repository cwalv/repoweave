//! What the `rwv sync-to` machine reported, and the `--json` envelope built
//! from that alone.
//!
//! A module of its own so that only [`SyncToRecorder`] writes an outcome's
//! fields: `sync.rs` is one module, and inside it privacy separates nothing.
//! On a resumed op the invocation and the machine name different workspaces,
//! and the envelope reports the machine's.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{
    ContainmentOutput, MemberOutput, OpCoordinates, OutputHandler, RepoSyncOutcome,
    RetiredWorkweave, Step3AdvanceOutput, SyncOutcomeOutput, SyncToJsonOutput,
    SYNC_TO_JSON_SCHEMA_URL,
};
use crate::manifest::{RepoKey, WorkweaveName};
use crate::op_state::OpPhase;
use crate::path_spelling::WirePath;
use crate::placement::PlacementOutput;
use crate::workspace::Resolution;

/// Everything the machine reported through its handler during one run.
#[derive(Default)]
pub(super) struct SyncToOutcome {
    records: Vec<SyncOutcomeOutput>,
    step3_advances: HashMap<String, Step3AdvanceOutput>,
    project_repo_advance: Option<Step3AdvanceOutput>,
    coordinates: Option<OpCoordinates>,
    retired: Option<WorkweaveName>,
    resumed: Option<OpPhase>,
    placements: Vec<PlacementOutput>,
    members: Vec<MemberOutput>,
    complete: bool,
}

impl SyncToOutcome {
    pub(super) fn any_failure(&self) -> bool {
        self.records.iter().any(SyncOutcomeOutput::is_failure)
    }

    /// `None` when there is nothing to describe: the machine refused before it
    /// resolved its coordinates, or `failed` before recording an outcome or
    /// running its last phase, where the error is the whole story.
    pub(super) fn envelope(
        self,
        failed: bool,
        resolution: Option<Resolution>,
    ) -> Option<SyncToJsonOutput> {
        if self.records.is_empty() && failed && !self.complete {
            return None;
        }
        let coordinates = self.coordinates?;
        let mut step3_advances = self.step3_advances;
        // A resume that re-entered downstream of replay records no outcomes,
        // so an advance it performs has no record to ride on.
        let outcomes = self
            .records
            .into_iter()
            .map(|mut outcome| {
                let path = match &outcome {
                    SyncOutcomeOutput::Converged { path, .. }
                    | SyncOutcomeOutput::AlreadyAhead { path, .. }
                    | SyncOutcomeOutput::NoOp { path, .. }
                    | SyncOutcomeOutput::Failed { path, .. } => path.clone(),
                };
                if let Some(advance) = step3_advances.remove(&path) {
                    *outcome.step3_advance_mut() = Some(advance);
                }
                outcome
            })
            .collect();
        Some(SyncToJsonOutput {
            schema: SYNC_TO_JSON_SCHEMA_URL.to_owned(),
            source_workweave: coordinates.source_workweave,
            target: WirePath::of(&coordinates.target),
            retired: self.retired.is_some(),
            resumed: self.resumed,
            outcomes,
            placements: self.placements,
            members: self.members,
            complete: self.complete,
            project_repo_advance: self.project_repo_advance,
            resolution,
        })
    }
}

/// The envelope-mode handler for `rwv sync-to --json`: it collects what the
/// machine reports into a [`SyncToOutcome`] and prints nothing.
#[derive(Default)]
pub(super) struct SyncToRecorder(Mutex<SyncToOutcome>);

impl SyncToRecorder {
    pub(super) fn into_outcome(self) -> SyncToOutcome {
        self.0.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    fn with(&self, report: impl FnOnce(&mut SyncToOutcome)) {
        report(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

impl OutputHandler for SyncToRecorder {
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
        self.with(|o| o.retired = Some(retired.name.clone()));
    }

    fn record_resumed(&self, phase: OpPhase) {
        self.with(|o| o.resumed = Some(phase));
    }

    fn record_complete(&self) {
        self.with(|o| o.complete = true);
    }
}
