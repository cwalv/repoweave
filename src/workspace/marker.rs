//! The marker's fields are private to this module, so [`WorkweaveMarker::new`]
//! is the one literal that sets them.

use crate::manifest::{ProjectName, WorkweaveName};
use crate::path_spelling::CanonicalPath;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Metadata written to `.rwv-workweave` in a workweave root.
///
/// Records the workweave's name, its relationship to the primary workspace
/// and the workspace it was forked from. The name is the marker's own, so a
/// lost index is rebuilt whole wherever the workweave's directory is.
///
/// `parent` is the workspace the workweave was created from: `primary` when
/// created from the primary, the parent workweave's path when created from
/// inside another workweave. Workweaves form a tree; `parent` is the edge a
/// bare `rwv land` lands along, one hop toward the primary.
///
/// All four fields (`primary`, `project`, `name`, `parent`) are required.
/// Written and read as JSON; a YAML marker, or one missing `parent` or
/// `name`, is a legacy marker, migrated by `rwv doctor --fix` or by the
/// first verb run inside a workweave its project's index lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkweaveMarker {
    primary: CanonicalPath,
    project: ProjectName,
    name: WorkweaveName,
    parent: CanonicalPath,
}

impl WorkweaveMarker {
    /// Build a marker for the workweave `name` of `primary`'s `project`,
    /// forked from `parent_source`.
    pub fn new(
        primary: &Path,
        project: ProjectName,
        name: WorkweaveName,
        parent_source: &Path,
    ) -> Self {
        WorkweaveMarker {
            primary: CanonicalPath::of(primary),
            project,
            name,
            parent: CanonicalPath::of(parent_source),
        }
    }

    pub fn primary(&self) -> &CanonicalPath {
        &self.primary
    }

    /// Whether `candidate` is the primary workspace this marker names.
    pub fn names_primary(&self, candidate: &CanonicalPath) -> bool {
        self.primary == *candidate
    }

    pub fn project(&self) -> &ProjectName {
        &self.project
    }

    pub fn name(&self) -> &WorkweaveName {
        &self.name
    }

    pub fn parent(&self) -> &CanonicalPath {
        &self.parent
    }

    pub fn repoint_parent(&mut self, parent_source: &Path) {
        self.parent = CanonicalPath::of(parent_source);
    }
}
