use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// The plugin reaches rwv only through its `--json` output, so this set is its
// own closed copy of rwv's roles: a role it does not name is output it cannot
// read.
/// A repo's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Owned,
    Fork,
    Dependency,
    Reference,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(token)) => f.write_str(&token),
            other => panic!("a fieldless enum must serialize to a string, got {other:?}"),
        }
    }
}

/// The envelope `rwv each --json` prints once every repo is done, whatever
/// `-j` is: `{ "$schema": "<url>", "outcomes": [<Outcome>, ...] }`, sorted
/// by repo path.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EachJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub outcomes: Vec<Outcome>,
}

/// What happened in one repo.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Outcome {
    /// The repo's manifest path.
    pub path: String,
    pub role: Role,
    pub status: EachOutcomeStatus,
    /// The command's exit code. Absent when it did not run, could not be
    /// started, or was ended by a signal.
    pub exit_code: Option<i32>,
    /// The signal that ended the command (Unix only).
    pub signal: Option<i32>,
    /// Wall-clock run time. Absent when the command did not run.
    pub duration_ms: Option<u64>,
    /// The command's standard output, decoded lossily as UTF-8.
    pub stdout: String,
    /// The command's standard error, decoded lossily as UTF-8. Holds the
    /// reason when the command could not be started.
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EachOutcomeStatus {
    /// The command exited 0.
    Ok,
    /// The command exited non-zero, was ended by a signal, or could not be
    /// started.
    Failed,
    /// `--timeout` elapsed and the command's process tree was killed.
    TimedOut,
    /// Never started: `--fail-fast` or an interrupt stopped new repos first.
    NotRun,
    /// The repo's checkout is absent from disk; `rwv fetch` restores it.
    NotPresent,
}
