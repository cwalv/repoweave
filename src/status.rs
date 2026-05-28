//! `rwv status` — per-repo state of the CWD workspace.

use crate::git::GitVcs;
use crate::manifest::Project;
use crate::vcs::{ResolvedRevisionId, Vcs};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Schema URL for `rwv status --json` output. Pins to the committed artifact
/// under `docs/reference/schemas/status.json`. Emitted as the top-level
/// `$schema` field of the [`StatusJsonOutput`] envelope.
pub const STATUS_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/status.json";

/// Top-level envelope for `rwv status --json`. Matches the convention adopted
/// by doctor (`$schema` + `violations`) and sync (`$schema` + `outcomes`):
/// `{ "$schema": "<url>", "repos": [<RepoStatus>, ...] }`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StatusJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub repos: Vec<RepoStatus>,
}

/// Relation between the current branch tip and the lock SHA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LockRelation {
    Ok,
    Ahead,
    Behind,
    Diverged,
    NoLock,
    Unknown,
}

impl std::fmt::Display for LockRelation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LockRelation::Ok => "ok",
            LockRelation::Ahead => "ahead",
            LockRelation::Behind => "behind",
            LockRelation::Diverged => "diverged",
            LockRelation::NoLock => "no-lock",
            LockRelation::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

/// Per-repo status entry.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RepoStatus {
    pub path: String,
    pub branch: Option<String>,
    pub tip: Option<String>,
    pub lock_sha: Option<String>,
    pub relation: LockRelation,
    pub mid_op: Option<String>,
    pub role: String,
    pub url: String,
    pub project: String,
    pub absolute_path: String,
}

fn compute_relation(
    repo_abs: &Path,
    tip: &Option<ResolvedRevisionId>,
    lock_sha: &Option<ResolvedRevisionId>,
) -> LockRelation {
    let (tip, lock) = match (tip, lock_sha) {
        (Some(t), Some(l)) => (t, l),
        (_, None) => return LockRelation::NoLock,
        (None, _) => return LockRelation::Unknown,
    };

    if tip == lock {
        return LockRelation::Ok;
    }

    let tip_ahead = GitVcs::is_ancestor(repo_abs, lock.as_str(), tip.as_str());
    let tip_behind = GitVcs::is_ancestor(repo_abs, tip.as_str(), lock.as_str());

    match (tip_ahead, tip_behind) {
        (true, _) => LockRelation::Ahead,
        (_, true) => LockRelation::Behind,
        _ => LockRelation::Diverged,
    }
}

fn project_names_for_ctx(ctx: &WorkspaceContext) -> Vec<String> {
    match &ctx.location {
        WorkspaceLocation::Weave { project: Some(p) } => vec![p.as_str().to_owned()],
        WorkspaceLocation::Workweave { project, .. } => vec![project.as_str().to_owned()],
        WorkspaceLocation::Weave { project: None } => {
            crate::workspace::discover_project_paths(ctx.active_path())
        }
    }
}

/// Run `rwv status` for the CWD workspace.
///
/// When `project_override` is `Some`, status is shown for that project
/// (does not change `.rwv-active`).
pub fn run_status(
    cwd: &Path,
    json: bool,
    project_override: Option<crate::manifest::ProjectName>,
) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, project_override)?;

    // When a specific project is named (via .rwv-active or --project), verify
    // it exists on disk before proceeding. When no project is active, the
    // no-active-project path in project_names_for_ctx falls back to listing
    // all projects — that path is fine and does not need the disk check here.
    if ctx.active_project().is_some() {
        ctx.require_active_project_on_disk()?;
    }

    let git = GitVcs;
    let workspace_dir = ctx.active_path().to_path_buf();

    let mut entries: Vec<RepoStatus> = Vec::new();

    for pname in project_names_for_ctx(&ctx) {
        let project_dir = workspace_dir.join("projects").join(&pname);
        let project = match Project::from_dir(&project_dir) {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Resolve lock entries against their on-disk repos so equality with
        // a tip ResolvedRevisionId (which always carries the canonical SHA) works
        // whether the lock pinned a tag, branch, or raw SHA.
        let lock = project
            .lock
            .map(|raw| raw.resolve_versions(&workspace_dir).0);

        for (repo_path, entry) in &project.manifest.repositories {
            let repo_abs = workspace_dir.join(repo_path.as_path());

            let branch = git
                .current_ref(&repo_abs)
                .ok()
                .flatten()
                .map(|r| r.as_str().to_owned());

            let tip = git.head_revision(&repo_abs).ok();

            let lock_sha = lock
                .as_ref()
                .and_then(|l| l.repositories.get(repo_path))
                .map(|e| e.version.clone());

            let relation = compute_relation(&repo_abs, &tip, &lock_sha);

            let mid_op = GitVcs::mid_op_state(&repo_abs);

            entries.push(RepoStatus {
                path: repo_path.to_string(),
                branch,
                tip: tip.map(|r| r.display_str().to_owned()),
                lock_sha: lock_sha.map(|r| r.display_str().to_owned()),
                relation,
                mid_op,
                role: entry.role.as_str().to_string(),
                url: entry.url.to_string(),
                project: pname.to_string(),
                absolute_path: repo_abs.to_string_lossy().to_string(),
            });
        }
    }

    if json {
        let envelope = StatusJsonOutput {
            schema_url: STATUS_SCHEMA_URL.to_string(),
            repos: entries,
        };
        let out = serde_json::to_string_pretty(&envelope)
            .map_err(|e| anyhow::anyhow!("failed to serialize status to JSON: {e}"))?;
        println!("{out}");
    } else {
        print_table(&entries);
    }

    Ok(())
}

fn print_table(entries: &[RepoStatus]) {
    // Measure column widths.
    let path_w = entries
        .iter()
        .map(|e| e.path.len())
        .max()
        .unwrap_or(0)
        .max(4);
    let branch_w = entries
        .iter()
        .map(|e| e.branch.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0)
        .max(6);
    let tip_w = entries
        .iter()
        .map(|e| e.tip.as_deref().unwrap_or("-").len().min(12))
        .max()
        .unwrap_or(0)
        .max(3);
    let lock_w = entries
        .iter()
        .map(|e| e.lock_sha.as_deref().unwrap_or("-").len().min(12))
        .max()
        .unwrap_or(0)
        .max(4);

    for entry in entries {
        let branch = entry.branch.as_deref().unwrap_or("-");
        let tip = entry
            .tip
            .as_deref()
            .map(|s| &s[..s.len().min(12)])
            .unwrap_or("-");
        let lock = entry
            .lock_sha
            .as_deref()
            .map(|s| &s[..s.len().min(12)])
            .unwrap_or("-");
        let mid = entry
            .mid_op
            .as_deref()
            .map(|s| format!("  [{s}]"))
            .unwrap_or_default();

        println!(
            "{:<path_w$}  {:<branch_w$}  {:<tip_w$}  lock: {:<lock_w$}  [{}]{}",
            entry.path,
            branch,
            tip,
            lock,
            entry.relation,
            mid,
            path_w = path_w,
            branch_w = branch_w,
            tip_w = tip_w,
            lock_w = lock_w,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip the envelope through serde so the `$schema` rename and the
    /// `repos` array shape are pinned independent of the live status path.
    #[test]
    fn status_json_envelope_round_trips() {
        let envelope = StatusJsonOutput {
            schema_url: STATUS_SCHEMA_URL.to_string(),
            repos: vec![RepoStatus {
                path: "github/org/repo".into(),
                branch: Some("main".into()),
                tip: Some("abc123".into()),
                lock_sha: Some("abc123".into()),
                relation: LockRelation::Ok,
                mid_op: None,
                role: "owned".into(),
                url: "https://example.com/repo.git".into(),
                project: "demo".into(),
                absolute_path: "/abs/github/org/repo".into(),
            }],
        };

        let json = serde_json::to_string(&envelope).expect("serializes");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parses");

        // Wire-shape: top-level $schema + repos array.
        assert_eq!(
            v["$schema"],
            serde_json::Value::String(STATUS_SCHEMA_URL.to_string())
        );
        let repos = v["repos"].as_array().expect("repos is an array");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["path"], "github/org/repo");
        assert_eq!(repos[0]["relation"], "ok");

        // Typed round-trip back to the envelope struct.
        let decoded: StatusJsonOutput = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded.schema_url, STATUS_SCHEMA_URL);
        assert_eq!(decoded.repos.len(), 1);
        assert_eq!(decoded.repos[0].path, "github/org/repo");
        assert_eq!(decoded.repos[0].relation, LockRelation::Ok);
    }
}
