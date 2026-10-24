//! The "known healthy at version" floor — the record that licenses
//! migratory-arm removal, and the gate that refuses an upgrade past a weave
//! that never recorded one.
//!
//! ## The marker (P1)
//!
//! One record per weave root (`.rwv-health-floor`, beside `.rwv-active`),
//! written by a CLEAN weave-wide `rwv doctor` run. It carries the rwv
//! version that observed the weave clean and the tip of each project repo
//! at the time — version lineage plus recorded tips, structural only. A
//! `recorded_at` timestamp is included for the operator's eyes and is
//! never consumed as policy.
//!
//! What "clean" means here, pinned: no error-severity finding rendered,
//! and no violation rwv can still repair — every violation standing in
//! the run is report-only. The floor licenses removal of MIGRATORY arms,
//! and every arm is an Auto or Consented repair, so the attestation
//! requires exactly the repairable set to be empty. Report-only warnings
//! — version skew across sovereign repos, submodules in a reference
//! checkout, a sibling weave's workweaves in the shared container — do
//! not block: no arm repairs them, and they are not always this weave's
//! to clear, so gating on them makes such a weave permanently floorless.
//! Warning-severity integration issues (a missing `.code-workspace`, an
//! unsurfaced symlink) do not block for the same reason — ecosystem
//! hygiene is a different axis. Advisories are display-only and do not block.
//!
//! The floor records only from the operator surface: text-mode
//! `rwv doctor` run weave-wide (`--all`), unfiltered. A project-scoped or
//! `--kind`-filtered run proves nothing about the weave; `--json` is a
//! machine-reading surface and does not mutate state. The floor only ever
//! ADVANCES: a clean run under an older binary than the recorded floor
//! refreshes nothing, so a stray downgrade cannot lower what a newer
//! version attested.
//!
//! ## The removal rule (P2, alpha regime)
//!
//! A migratory arm repairing states written before version X may be removed
//! once every OWNED weave's floor records clean at >= X — the owned-weave
//! checklist is the operator's, pre-v1. When an arm is removed, the release
//! names its floor requirement in [`ACTIVE_REQUIREMENT`]; a binary carrying
//! a requirement refuses to run doctor against a weave whose floor is below
//! it, naming the bridge version to step through — and the bridge is
//! runnable by construction, because the named version still carries the
//! arm (the 7kal clause: never name a remedy the broken state blocks).
//! No requirement is active while every migratory arm still ships. The
//! window parameterization is deliberately absent until v1.

use crate::state_file::{StateFile, StatePath};
use crate::vcs::CommitId;
use anyhow::Context;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// File name of the floor record, at the weave root.
pub const FLOOR_FILE: &str = ".rwv-health-floor";

/// The recorded floor: which rwv version last observed this weave clean,
/// and where each project repo stood when it did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthFloor {
    /// The rwv version that observed the weave clean.
    #[serde(with = "version_text")]
    pub version: Version,
    /// HEAD of each project repo at the time, keyed by project name — the
    /// structural anchor for what the attestation was about.
    #[serde(default)]
    pub project_tips: BTreeMap<String, CommitId>,
    /// RFC3339, for the operator's eyes only. Never consumed as policy —
    /// every decision here is version lineage against a recorded floor.
    #[serde(default)]
    pub recorded_at: Option<String>,
}

/// A floor requirement a release declares when a migratory arm is removed.
pub struct FloorRequirement {
    /// The minimum recorded floor version this binary can serve.
    pub minimum: Version,
    /// The version to step through: the newest release that still carries
    /// every arm a below-minimum weave might need. Must be runnable — it is
    /// the remedy the refusal names.
    pub bridge: Version,
}

/// The requirement this binary ships with. `None` while every migratory arm
/// is still present — which is the case until the first P2 removal names
/// its floor here.
pub const ACTIVE_REQUIREMENT: Option<FloorRequirement> = None;

fn floor_path(ws_root: &Path) -> StatePath {
    StateFile::HealthFloor.in_dir(ws_root)
}

/// Read the weave's floor record. `Ok(None)` when none was ever recorded;
/// an unreadable or unparseable file also reads as `None` (the conservative
/// direction: an illegible floor licenses nothing). A record whose version is
/// not a semver is unparseable.
pub fn read(ws_root: &Path) -> Option<HealthFloor> {
    let raw = floor_path(ws_root).read_to_string().ok()?;
    serde_json::from_str(&raw).ok()
}

/// Record a clean weave-wide doctor observation under the running version.
///
/// Advance-only: when the recorded floor's version is newer than the
/// running binary's, the record is left alone. An equal version refreshes
/// the tips. Failure to write is reported to the caller but is the
/// caller's to soften — a floor is a record, not a precondition of the run
/// that earned it.
pub fn record_clean_run(ws_root: &Path, vcs: &dyn crate::vcs::Vcs) -> anyhow::Result<()> {
    let running = crate::rwv_version();
    if read(ws_root).is_some_and(|existing| existing.version > *running) {
        return Ok(());
    }

    let mut project_tips = BTreeMap::new();
    for project in crate::workspace::discover_projects(ws_root) {
        let repo = crate::workspace::project_dir(ws_root, &project);
        if let Ok(tip) = vcs.head_revision(&repo) {
            project_tips.insert(project.as_str().to_string(), tip.id().clone());
        }
    }

    let floor = HealthFloor {
        version: running.clone(),
        project_tips,
        recorded_at: Some(crate::op_state::utc_now_rfc3339()),
    };
    let bytes = serde_json::to_vec_pretty(&floor).context("serialize health floor")?;
    floor_path(ws_root)
        .publish(&bytes)
        .context("write health floor")?;
    Ok(())
}

/// Enforce this binary's [`ACTIVE_REQUIREMENT`] against the weave's floor.
/// A no-op while no requirement is active.
pub fn enforce(ws_root: &Path) -> anyhow::Result<()> {
    match &ACTIVE_REQUIREMENT {
        None => Ok(()),
        Some(req) => enforce_with(read(ws_root).as_ref(), req),
    }
}

/// The refusal itself, separated so the rule is testable while no
/// requirement ships: a floor below the requirement — or no floor at all —
/// refuses, naming the bridge version to step through.
pub fn enforce_with(floor: Option<&HealthFloor>, req: &FloorRequirement) -> anyhow::Result<()> {
    let state = match floor {
        Some(f) if f.version >= req.minimum => return Ok(()),
        Some(f) => format!("records version {}", f.version),
        None => "records no clean run".to_string(),
    };
    crate::refuse!(
        crate::refusal::RefusalKind::HealthFloorTooLow,
        "this weave's health floor {state}, below the {min} this rwv \
         requires: migratory repair arms for older states were removed \
         in this release. Step through version {bridge}: install rwv \
         {bridge}, run `rwv doctor --fix --all` until `rwv doctor --all` \
         reports clean (which records the floor), then upgrade again",
        min = req.minimum,
        bridge = req.bridge,
    )
}

mod version_text {
    use semver::Version;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(version: &Version, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(version)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Version, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor(version: &str) -> HealthFloor {
        HealthFloor {
            version: version.parse().unwrap(),
            project_tips: BTreeMap::new(),
            recorded_at: None,
        }
    }

    const REQ: FloorRequirement = FloorRequirement {
        minimum: Version::new(0, 18, 0),
        bridge: Version::new(0, 18, 0),
    };

    #[test]
    fn a_floor_round_trips_its_version_as_semver_text() {
        let text = serde_json::to_string(&floor("0.18.0")).unwrap();
        assert!(text.contains(r#""version":"0.18.0""#), "got: {text}");
        let back: HealthFloor = serde_json::from_str(&text).unwrap();
        assert_eq!(back.version, Version::new(0, 18, 0));
    }

    #[test]
    fn a_floor_whose_version_is_not_semver_does_not_parse() {
        for version in [
            "v0.18.0",
            "v0.18.0-86-gd52f24e-dirty",
            "d52f24e",
            "junk",
            "",
        ] {
            let record = format!(r#"{{"version":"{version}"}}"#);
            assert!(
                serde_json::from_str::<HealthFloor>(&record).is_err(),
                "{version:?} must not read as a floor version"
            );
        }
    }

    #[test]
    fn a_floor_whose_tip_is_not_a_commit_id_reads_as_absent() {
        let tmp = crate::tempdir().unwrap();
        let record =
            |tip: &str| format!(r#"{{"version":"0.18.0","project_tips":{{"app":"{tip}"}}}}"#);
        let write = |tip: &str| {
            floor_path(tmp.path())
                .publish(record(tip).as_bytes())
                .unwrap()
        };

        write(&"ab".repeat(20));
        assert!(read(tmp.path()).is_some(), "a commit id reads as a floor");

        write("v1.0");
        assert!(read(tmp.path()).is_none(), "a tag name is not a tip");
    }

    #[test]
    fn a_floor_at_or_above_the_minimum_passes() {
        assert!(enforce_with(Some(&floor("0.18.0")), &REQ).is_ok());
        assert!(enforce_with(Some(&floor("0.19.2")), &REQ).is_ok());
    }

    #[test]
    fn a_floor_below_the_minimum_refuses_naming_the_bridge() {
        let err = enforce_with(Some(&floor("0.17.0")), &REQ).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Step through version 0.18.0"),
            "the refusal names the runnable bridge; got: {msg}"
        );
        assert!(
            msg.contains("0.17.0"),
            "the refusal names the recorded floor; got: {msg}"
        );
    }

    #[test]
    fn a_missing_floor_refuses_naming_the_bridge() {
        let err = enforce_with(None, &REQ).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("records no clean run") && msg.contains("Step through version 0.18.0"),
            "an absent floor is below every requirement; got: {msg}"
        );
    }
}
