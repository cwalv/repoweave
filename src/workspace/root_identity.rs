//! The identity payloads' fields are private to this module, so
//! [`RootObservation::require_exclusive`] is their one producer.

use super::{active_project_path, ActivePointer, RootObservation, WorkweaveMarker};
use crate::manifest::ProjectName;
use crate::refusal::RefusalKind;
use anyhow::Context;
use std::path::PathBuf;

impl RootObservation {
    /// The identity a verb may act on, or the refusal that names the repair.
    pub fn require_exclusive(self) -> anyhow::Result<WeaveRootIdentity> {
        match self {
            RootObservation::Workweave { marker } => {
                Ok(WeaveRootIdentity::Workweave(WorkweaveIdentity { marker }))
            }
            RootObservation::Primary { root, pointer } => {
                Ok(WeaveRootIdentity::Primary(PrimaryIdentity {
                    root,
                    pointer,
                }))
            }
            RootObservation::Disputed { root, .. } => crate::refuse!(
                RefusalKind::WeaveRootIdentityConflict,
                "{} and {} both exist: a weave root carries the workweave marker \
                 or the active-project pointer, never both. Run `rwv doctor --fix`; \
                 it removes the redundant file where the primary-side workweave \
                 registry names this tree, and reports the conflict where nothing \
                 outside the tree settles which file is the stray.",
                WorkweaveMarker::path_in(&root),
                active_project_path(&root)
            ),
            RootObservation::MarkerUnverifiable {
                marker_path,
                defect,
                ..
            } => Err(crate::refusal::refusal(
                defect.kind(),
                defect.refusal(&marker_path),
            )),
        }
    }
}

/// The identity of a weave root that carries exactly one identity file.
///
/// [`RootObservation::require_exclusive`] is the only producer, and the arms
/// it refuses have no representation here. Both payloads hold private fields
/// for that reason: a consumer that could assemble one from a marker and a
/// pointer would be re-deciding, at its own site, the tiebreak this projection
/// exists to have already refused.
#[derive(Debug)]
pub enum WeaveRootIdentity {
    Workweave(WorkweaveIdentity),
    Primary(PrimaryIdentity),
}

#[derive(Debug)]
pub struct WorkweaveIdentity {
    marker: WorkweaveMarker,
}

impl WorkweaveIdentity {
    pub fn into_marker(self) -> WorkweaveMarker {
        self.marker
    }
}

/// A primary root, and the only thing that can select a project.
///
/// The root travels inside the witness rather than beside it. A witness that
/// only attested "some primary root exists" would leave the directory a free
/// argument at the write, so the caller could still name a marker root and
/// nothing in the type would object.
#[derive(Debug, Clone)]
pub struct PrimaryIdentity {
    root: PathBuf,
    pointer: ActivePointer,
}

impl PrimaryIdentity {
    /// The `.rwv-active` this root carried when it was observed.
    pub fn pointer(&self) -> &ActivePointer {
        &self.pointer
    }

    /// Write the `.rwv-active` pointer — project SELECTION, and rwv's sole
    /// write path for it.
    ///
    /// Selection is primary-only: a workweave's project is fixed at creation
    /// by its `.rwv-workweave` marker and cannot be switched, so a pointer
    /// there would be a second, unread copy of the workweave's own identity
    /// beside the marker — the state `rwv doctor` reports as
    /// [`CheckViolation::WeaveRootIdentityConflict`]. That is why this hangs
    /// off the witness instead of taking a root: the arm that carries a
    /// marker has no such method, so the write is not expressible there.
    ///
    /// [`CheckViolation::WeaveRootIdentityConflict`]: crate::check::CheckViolation::WeaveRootIdentityConflict
    pub fn select_project(&self, project: &ProjectName) -> anyhow::Result<()> {
        let path = active_project_path(&self.root);
        path.publish(format!("{}\n", project.as_str()).as_bytes())
            .with_context(|| format!("failed to write {path}"))
    }
}
