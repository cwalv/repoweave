//! The marker's fields are private to this module, so [`WorkweaveMarker::new`]
//! is the one literal that sets them.

use crate::manifest::ProjectName;
use crate::path_spelling::CanonicalPath;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Metadata written to `.rwv-workweave` in a workweave root.
///
/// Records the relationship to the primary workspace and the workspace this
/// workweave was forked from.
///
/// `parent` is the workspace the workweave was created from: `primary` when
/// created from the primary, the parent workweave's path when created from
/// inside another workweave. Workweaves form a tree; `parent` is the edge a
/// bare `rwv sync-to` lands along, one hop toward the primary.
///
/// All three fields (`primary`, `project`, `parent`) are required. Written
/// and read as JSON; a YAML marker, or one written before `parent` was
/// introduced, is a legacy marker and must be migrated with
/// `rwv doctor --fix` before the workweave can be used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkweaveMarker {
    primary: CanonicalPath,
    project: ProjectName,
    parent: CanonicalPath,
}

impl WorkweaveMarker {
    /// Build a marker for a workweave of `primary`'s, forked from
    /// `parent_source`.
    pub fn new(primary: &Path, project: ProjectName, parent_source: &Path) -> Self {
        WorkweaveMarker {
            primary: CanonicalPath::of(primary),
            project,
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

    pub fn parent(&self) -> &CanonicalPath {
        &self.parent
    }

    pub fn repoint_parent(&mut self, parent_source: &Path) {
        self.parent = CanonicalPath::of(parent_source);
    }
}
