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
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// Schema URL embedded in each placement line a `--json -j N` run streams.
/// Pins to `docs/reference/schemas/placement-record.json`, shared by every
/// verb that places files.
pub const PLACEMENT_RECORD_SCHEMA_URL: &str = crate::schema_url::schema_url!("placement-record");

/// How the files of one checkout were placed when a verb created or moved it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementOutput {
    /// Manifest-relative path of the repository.
    pub path: String,
    /// The checkout whose files were placed.
    pub absolute_path: String,
    /// How many files were cloned, linked or written.
    pub files: FilePlacement,
}

impl PlacementOutput {
    pub(crate) fn new(repo_path: &RepoPath, checkout: &Path, files: FilePlacement) -> Self {
        Self {
            path: repo_path.to_string(),
            absolute_path: crate::path_spelling::wire_path(checkout),
            files,
        }
    }

    /// Write this placement to stdout as one NDJSON line, under `stdout_lock`
    /// so concurrent writers cannot interleave.
    pub(crate) fn stream(&self, stdout_lock: &Mutex<()>) {
        let record = PlacementNdjsonRecord {
            schema: PLACEMENT_RECORD_SCHEMA_URL,
            placement: self,
        };
        if let Ok(line) = serde_json::to_string(&record) {
            let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            let _ = writeln!(handle, "{line}");
            let _ = handle.flush();
        }
    }
}

/// One placement line streamed by `--json -j N` with `N > 1`, beside the
/// verb's own per-repo lines: a flat object whose `$schema` names
/// `placement-record.json`, which is how a consumer tells the two apart.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlacementNdjsonRecord<'a> {
    #[serde(rename = "$schema")]
    pub schema: &'a str,
    #[serde(flatten)]
    pub placement: &'a PlacementOutput,
}
