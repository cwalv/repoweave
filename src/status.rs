//! `rwv status` — per-repo state of the CWD workspace.

use crate::git::GitVcs;
use crate::manifest::Project;
use crate::vcs::{RevisionId, Vcs};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use serde::Serialize;
use std::path::Path;

/// Relation between the current branch tip and the lock SHA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Serialize)]
pub struct RepoStatus {
    pub path: String,
    pub branch: Option<String>,
    pub tip: Option<String>,
    pub lock_sha: Option<String>,
    pub relation: LockRelation,
    pub mid_op: Option<String>,
}

fn compute_relation(
    repo_abs: &Path,
    tip: &Option<RevisionId>,
    lock_sha: &Option<RevisionId>,
) -> LockRelation {
    let (tip, lock) = match (tip, lock_sha) {
        (Some(t), Some(l)) => (t.as_str(), l.as_str()),
        (_, None) => return LockRelation::NoLock,
        (None, _) => return LockRelation::Unknown,
    };

    if tip == lock {
        return LockRelation::Ok;
    }

    let tip_ahead = GitVcs::is_ancestor(repo_abs, lock, tip);
    let tip_behind = GitVcs::is_ancestor(repo_abs, tip, lock);

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
            crate::workspace::discover_project_paths(&ctx.root)
        }
    }
}

/// Run `rwv status` for the CWD workspace.
pub fn run_status(cwd: &Path, json: bool) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let git = GitVcs;
    let workspace_dir = ctx.resolve_path().to_path_buf();

    let mut entries: Vec<RepoStatus> = Vec::new();

    for pname in project_names_for_ctx(&ctx) {
        let project_dir = ctx.root.join("projects").join(&pname);
        let project = match Project::from_dir(&project_dir) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let lock = project.lock;

        for repo_path in project.manifest.repositories.keys() {
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
                tip: tip.map(|r| r.as_str().to_owned()),
                lock_sha: lock_sha.map(|r| r.as_str().to_owned()),
                relation,
                mid_op,
            });
        }
    }

    if json {
        let out = serde_json::to_string_pretty(&entries)
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
