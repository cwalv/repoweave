//! The "known healthy at version" floor — the record that licenses
//! migratory-arm removal, and the gate that refuses an upgrade past a weave
//! that never recorded one.
//!
//! ## The marker (P1)
//!
//! One record per weave root (`.rwv-health-floor`, beside `.rwv-active`),
//! written by a CLEAN weave-wide `rwv doctor` run: exit 0 AND zero
//! violations after reclassification. It carries the rwv version that
//! observed the weave clean and the tip of each project repo at the time —
//! version lineage plus recorded tips, structural only. A `recorded_at`
//! timestamp is included for the operator's eyes and is never consumed as
//! policy.
//!
//! What "clean" means here, pinned: the VIOLATIONS list is empty (the same
//! records `--json` carries in `violations[]`, itemized or count-collapsed
//! in text) and no error-severity issue rendered. Warning-severity
//! integration issues (a missing `.code-workspace`, an unsurfaced symlink)
//! do not block the floor: the floor licenses removal of MIGRATORY arms,
//! and migratory arms repair violations — ecosystem hygiene is a different
//! axis, and gating on it would make floors unrecordable on any weave with
//! an integration warning. Advisories are display-only and do not block.
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

use anyhow::Context;
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
    pub version: String,
    /// HEAD of each project repo at the time, keyed by project name — the
    /// structural anchor for what the attestation was about.
    #[serde(default)]
    pub project_tips: BTreeMap<String, String>,
    /// RFC3339, for the operator's eyes only. Never consumed as policy —
    /// every decision here is version lineage against a recorded floor.
    #[serde(default)]
    pub recorded_at: Option<String>,
}

/// A floor requirement a release declares when a migratory arm is removed.
pub struct FloorRequirement {
    /// The minimum recorded floor version this binary can serve.
    pub minimum: &'static str,
    /// The version to step through: the newest release that still carries
    /// every arm a below-minimum weave might need. Must be runnable — it is
    /// the remedy the refusal names.
    pub bridge: &'static str,
}

/// The requirement this binary ships with. `None` while every migratory arm
/// is still present — which is the case until the first P2 removal names
/// its floor here.
pub const ACTIVE_REQUIREMENT: Option<FloorRequirement> = None;

/// Read the weave's floor record. `Ok(None)` when none was ever recorded;
/// an unreadable or unparseable file also reads as `None` (the conservative
/// direction: an illegible floor licenses nothing).
pub fn read(ws_root: &Path) -> Option<HealthFloor> {
    let raw = std::fs::read_to_string(ws_root.join(FLOOR_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The leading `major.minor.patch` triple of a version string, tolerating a
/// `git describe` tail and tag prefix (`v0.16.0-3-ge5bfa9f`).
fn version_triple(v: &str) -> Option<(u64, u64, u64)> {
    let head = v.split('-').next()?;
    let head = head.strip_prefix('v').unwrap_or(head);
    let mut parts = head.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
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
    if let Some(existing) = read(ws_root) {
        if let (Some(old), Some(new)) = (version_triple(&existing.version), version_triple(running))
        {
            if old > new {
                return Ok(());
            }
        }
    }

    let mut project_tips = BTreeMap::new();
    for project in crate::workweave_index::projects_on_disk(ws_root) {
        let repo = crate::workspace::project_dir(ws_root, project.as_str());
        if let Ok(tip) = vcs.head_revision(&repo) {
            project_tips.insert(project.as_str().to_string(), tip.as_str().to_string());
        }
    }

    let floor = HealthFloor {
        version: running.to_string(),
        project_tips,
        recorded_at: Some(crate::op_state::utc_now_rfc3339()),
    };
    let bytes = serde_json::to_vec_pretty(&floor).context("serialize health floor")?;
    crate::durable_file::replace(&ws_root.join(FLOOR_FILE), &bytes)
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
    let minimum = version_triple(req.minimum)
        .context("a floor requirement's minimum must be a version triple")?;
    let recorded = floor.and_then(|f| version_triple(&f.version));
    match recorded {
        Some(v) if v >= minimum => Ok(()),
        Some(_) | None => {
            let state = match floor {
                Some(f) => format!("records version {}", f.version),
                None => "records no clean run".to_string(),
            };
            anyhow::bail!(
                "this weave's health floor {state}, below the {min} this rwv \
                 requires: migratory repair arms for older states were removed \
                 in this release. Step through version {bridge}: install rwv \
                 {bridge}, run `rwv doctor --fix --all` until `rwv doctor --all` \
                 reports clean (which records the floor), then upgrade again",
                min = req.minimum,
                bridge = req.bridge,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_triple_tolerates_describe_tails() {
        assert_eq!(version_triple("0.16.0-3-ge5bfa9f"), Some((0, 16, 0)));
        assert_eq!(
            version_triple("v0.18.0-86-gd52f24e-dirty"),
            Some((0, 18, 0))
        );
        assert_eq!(version_triple("0.18.0"), Some((0, 18, 0)));
        assert_eq!(version_triple("junk"), None);
    }

    fn floor(version: &str) -> HealthFloor {
        HealthFloor {
            version: version.into(),
            project_tips: BTreeMap::new(),
            recorded_at: None,
        }
    }

    const REQ: FloorRequirement = FloorRequirement {
        minimum: "0.18.0",
        bridge: "0.18.0",
    };

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
