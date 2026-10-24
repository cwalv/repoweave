//! How one checkout's files were placed, as `--json` reports it.
//!
//! A verb's outcome records never carry file counts: each placement is its own
//! record naming the checkout it describes, so a verb that places files in
//! more than one workspace, or after an outcome was written, reports every
//! placement the same way.

use crate::manifest::RepoPath;
use crate::share::FilePlacement;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How the files of one checkout were placed when a verb created or moved it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementOutput {
    /// Manifest-relative path of the repository.
    pub path: String,
    /// The checkout whose files were placed.
    pub absolute_path: crate::path_spelling::WirePath,
    /// How many files were cloned, linked or written.
    pub files: FilePlacement,
}

impl PlacementOutput {
    pub(crate) fn new(repo_path: &RepoPath, checkout: &Path, files: FilePlacement) -> Self {
        Self {
            path: repo_path.to_string(),
            absolute_path: crate::path_spelling::WirePath::of(checkout),
            files,
        }
    }
}
