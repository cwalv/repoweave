//! `rwv status` — per-repo state of the CWD workspace.

use crate::git::GitVcs;
use crate::manifest::Project;
use crate::vcs::{ResolvedRevisionId, Vcs};
use crate::workspace::{Checkout, Resolution, WorkspaceContext};
use anyhow::Context;
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
    /// Resolved workspace coordinates (workspace root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// Relation between the current branch tip and the lock SHA.
///
/// The two clone-health variants (`Missing` / `Unreachable`) address distinct
/// failure modes that `NoLock` previously masked:
///
/// - `Missing` — the clone directory is absent from disk entirely (out-of-band
///   `rm -rf`, never fetched, etc.). The lock entry may be fine; the repair
///   verb is a re-clone / `rwv fetch`.
///
/// - `Unreachable` — the clone directory exists but the SHA pinned in the lock
///   is not present in the local object store (history rewritten, shallow
///   clone, object pruned). The repair verb is a `git fetch` / `rwv fetch`
///   to re-materialise the missing object.
///
/// Neither state should be attributed to the lock file itself — surfacing them
/// as `no-lock` misdirects operators at the wrong repair path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LockRelation {
    Ok,
    Ahead,
    Behind,
    Diverged,
    NoLock,
    Unknown,
    /// Clone directory is absent from disk (out-of-band removal, never fetched).
    /// Repair: re-clone / `rwv fetch`.
    Missing,
    /// Clone directory exists but the locked SHA is not in the local object
    /// store (history rewritten, shallow clone, object pruned).
    /// Repair: `git fetch` / `rwv fetch` to materialise the missing object.
    Unreachable,
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
            LockRelation::Missing => "missing",
            LockRelation::Unreachable => "unreachable",
        };
        f.write_str(s)
    }
}

/// Recorded-parent exposure for a per-repo status entry.
///
/// Parent identity comes from the workweave's `.rwv-workweave` marker
/// (`parent:`), NOT from the branch name: workweave branches are stacked
/// (`lab--wwb/lab--wwa/main`), so a constructed `basename(parent)/main` name
/// silently breaks for a workweave whose parent is itself a workweave, and is
/// also wrong after adoption re-points the parent to primary. Consumers that
/// need the parent must read this field, never reconstruct it from `branch`.
///
/// `path` is the recorded parent workspace path (identical for every repo in
/// the workweave). `tip` is this specific repo's parent tip — the SHA that
/// `git rev-parse HEAD` yields in the parent's checkout of the SAME repo — or
/// `None` when the parent has no checkout of this repo (or HEAD is
/// unreadable). The tip is what `git log <parent-tip>..HEAD` needs to compute
/// the workweave's unique commits without re-deriving branch layout.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ParentInfo {
    /// Recorded parent workspace path (from the `.rwv-workweave` marker).
    pub path: String,
    /// This repo's HEAD in the parent's checkout, if resolvable.
    pub tip: Option<String>,
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
    /// Recorded parent (path + per-repo parent tip) when CWD is a workweave;
    /// `None` in the primary weave (no marker, hence no recorded parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ParentInfo>,
}

/// Classify a repo's HEAD tip against its lock SHA into a [`LockRelation`].
///
/// The single ancestry gate the sync engine relies on. Relations
/// are named from the TIP's vantage (the opposite of "lock behind HEAD" prose):
/// `ahead` means the tip is a strict descendant of the lock (new commits since
/// last lock — the benign in-progress shape), `behind` means the tip is a strict
/// ancestor of the lock (a reset or an `update` without FF), `diverged` means
/// neither is an ancestor of the other. Exposed `pub(crate)` so `sync` can reuse
/// this exact vocabulary rather than inventing a parallel enum.
pub(crate) fn compute_relation(
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

/// The name of the ref `repo_abs`'s checkout is on, for the `branch` column.
///
/// Status asks [`Vcs::head_attachment`] only what a checkout is on: it never
/// writes a ref, so it wants no witness and no receipt.
///
/// What each arm means:
///
/// - `Attached` / `Unborn` — HEAD is symbolic, so a name is what the column
///   is for. Unborn still reports its name: a branch with no commits yet is
///   still the branch the checkout is on, and the empty `tip` column is what
///   says there is nothing on it.
/// - `Detached` — no ref names this checkout; the column renders `-`.
/// - `Err` — `NotARepo` or an unreadable ref database. Still `-`, because
///   the `branch` field is `Option<String>` in the committed JSON schema and
///   this is a report, not a gate: `relation` already carries the health
///   signal (`Missing` / `Unreachable`) that tells an operator which repair
///   verb to reach for. Distinguishing the two in the *output* would be a
///   schema change.
fn checkout_branch(git: &GitVcs, repo_abs: &Path) -> Option<String> {
    use crate::vcs::HeadAttachment;
    match git.head_attachment(repo_abs) {
        Ok(HeadAttachment::Attached(a)) => Some(a.to_string()),
        Ok(HeadAttachment::Unborn(u)) => Some(u.name().as_str().to_owned()),
        Ok(HeadAttachment::Detached(_)) | Err(_) => None,
    }
}

fn project_names_for_ctx(ctx: &WorkspaceContext) -> Vec<String> {
    match &ctx.checkout {
        Checkout::Primary { project: Some(p) } => vec![p.as_str().to_owned()],
        Checkout::Workweave { project, .. } => vec![project.as_str().to_owned()],
        Checkout::Primary { project: None } => {
            crate::workspace::discover_project_paths(ctx.active_path())
        }
    }
}

/// Run `rwv status` for the resolved invocation context.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_status(ctx: &WorkspaceContext, json: bool) -> anyhow::Result<()> {
    // When a specific project is named (via .rwv-active or --project), verify
    // it exists on disk before proceeding. When no project is active, the
    // no-active-project path in project_names_for_ctx falls back to listing
    // all projects — that path is fine and does not need the disk check here.
    if ctx.active_project().is_some() {
        ctx.require_active_project_on_disk()?;
    }

    let git = GitVcs;
    let workspace_dir = ctx.active_path().to_path_buf();

    // Recorded parent path from the `.rwv-workweave` marker (workweave-level;
    // identical for every repo). `None` in the primary weave, where there is
    // no marker and hence no recorded parent. Read from the marker — never
    // reconstructed from a branch name (which is wrong for stacked or
    // adopted-to-primary parents).
    let recorded_parent: Option<std::path::PathBuf> =
        crate::workspace::WorkweaveMarker::read(&workspace_dir)
            .ok()
            .flatten()
            .map(|m| m.parent);

    let mut entries: Vec<RepoStatus> = Vec::new();

    for pname in project_names_for_ctx(ctx) {
        let project_dir = workspace_dir.join("projects").join(&pname);
        let project = match Project::from_dir(&project_dir) {
            Ok(p) => p,
            Err(e) => {
                // Warn and skip; run `rwv doctor` to get the canonical
                // `unparseable-project` violation with full detail.
                eprintln!(
                    "warning: skipping project {pname}: manifest unreadable ({e}); \
                     run `rwv doctor` for details"
                );
                continue;
            }
        };
        // Resolve lock entries against their on-disk repos so equality with
        // a tip ResolvedRevisionId (which always carries the canonical SHA) works
        // whether the lock pinned a tag, branch, or raw SHA.
        //
        // We keep the raw lock alongside the resolved form so we can:
        //   (a) show the lock version even for repos whose clone is missing,
        //   (b) detect when a lock entry exists but the SHA is unreachable
        //       in the local object store (resolve_versions puts those in
        //       `failures` rather than including them in the resolved map).
        let (lock, lock_resolve_failures) = match project.lock {
            Some(raw) => {
                let raw_clone = raw.clone();
                let (resolved, failures) = raw.resolve_versions(&workspace_dir);
                (Some((raw_clone, resolved)), failures)
            }
            None => (None, Vec::new()),
        };

        for (repo_path, entry) in &project.manifest.repositories {
            let repo_abs = workspace_dir.join(repo_path.as_path());

            // --- clone-health pre-check ---
            //
            // Detect the two states that previously both collapsed into
            // `NoLock` (misdirecting operators at the lock file instead of
            // the clone):
            //
            //   Missing     — clone dir absent from disk entirely.  The lock
            //                 entry may be fine; repair = re-clone / rwv fetch.
            //
            //   Unreachable — clone dir present but the locked SHA is not in
            //                 the local object store (history rewritten, shallow
            //                 clone, object pruned).  Repair = git/rwv fetch.
            //
            // Either state short-circuits to an explicit relation before the
            // normal tip/lock comparison runs.

            if !repo_abs.exists() {
                // Clone directory is absent.  Retrieve the raw lock version
                // for the `lock_sha` display field (the resolved map omits
                // missing repos, but the raw lock still has the entry).
                let raw_lock_sha = lock
                    .as_ref()
                    .and_then(|(raw, _)| raw.get_entry(repo_path))
                    .map(|e| e.version.as_str().to_owned());
                entries.push(RepoStatus {
                    path: repo_path.to_string(),
                    branch: None,
                    tip: None,
                    lock_sha: raw_lock_sha,
                    relation: LockRelation::Missing,
                    mid_op: None,
                    role: entry.role.as_str().to_string(),
                    url: entry.url.to_string(),
                    project: pname.to_string(),
                    absolute_path: repo_abs.to_string_lossy().to_string(),
                    parent: recorded_parent.as_ref().map(|parent_path| ParentInfo {
                        path: parent_path.to_string_lossy().to_string(),
                        tip: None,
                    }),
                });
                continue;
            }

            // Clone dir exists — check whether the locked SHA is unreachable
            // in the local object store.  `resolve_versions` puts such entries
            // in `failures`; they are absent from the resolved lock map.
            if lock_resolve_failures.iter().any(|(p, _)| p == repo_path) {
                // The raw lock has an entry for this repo (it was in
                // `failures`) but the SHA cannot be resolved on disk.
                let raw_lock_sha = lock
                    .as_ref()
                    .and_then(|(raw, _)| raw.get_entry(repo_path))
                    .map(|e| e.version.as_str().to_owned());

                let branch = checkout_branch(&git, &repo_abs);

                let tip = git.head_revision(&repo_abs).ok();

                let mid_op = GitVcs::mid_op_state(&repo_abs);

                let parent = recorded_parent.as_ref().map(|parent_path| {
                    let parent_repo_abs = parent_path.join(repo_path.as_path());
                    let parent_tip = git
                        .head_revision(&parent_repo_abs)
                        .ok()
                        .map(|r| r.as_str().to_owned());
                    ParentInfo {
                        path: parent_path.to_string_lossy().to_string(),
                        tip: parent_tip,
                    }
                });

                entries.push(RepoStatus {
                    path: repo_path.to_string(),
                    branch,
                    tip: tip.map(|r| r.display_str().to_owned()),
                    lock_sha: raw_lock_sha,
                    relation: LockRelation::Unreachable,
                    mid_op,
                    role: entry.role.as_str().to_string(),
                    url: entry.url.to_string(),
                    project: pname.to_string(),
                    absolute_path: repo_abs.to_string_lossy().to_string(),
                    parent,
                });
                continue;
            }

            let branch = checkout_branch(&git, &repo_abs);

            let tip = git.head_revision(&repo_abs).ok();

            let lock_sha = lock
                .as_ref()
                .and_then(|(_, resolved)| resolved.get_entry(repo_path))
                .map(|e| e.version.clone());

            let relation = compute_relation(&repo_abs, &tip, &lock_sha);

            let mid_op = GitVcs::mid_op_state(&repo_abs);

            // Per-repo parent tip: resolve THIS repo's HEAD in the parent's
            // checkout of the same repo path. Read from the recorded parent
            // path, not a reconstructed branch name.
            let parent = recorded_parent.as_ref().map(|parent_path| {
                let parent_repo_abs = parent_path.join(repo_path.as_path());
                let parent_tip = git
                    .head_revision(&parent_repo_abs)
                    .ok()
                    .map(|r| r.as_str().to_owned());
                ParentInfo {
                    path: parent_path.to_string_lossy().to_string(),
                    tip: parent_tip,
                }
            });

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
                parent,
            });
        }
    }

    if json {
        let envelope = StatusJsonOutput {
            schema_url: STATUS_SCHEMA_URL.to_string(),
            repos: entries,
            resolution: ctx.resolution(),
        };
        let out = serde_json::to_string_pretty(&envelope)
            .context("failed to serialize status to JSON")?;
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
                parent: None,
            }],
            resolution: None,
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
        // A repo with no recorded parent omits the field entirely.
        assert!(
            repos[0].get("parent").is_none(),
            "parent field should be omitted when None; got: {}",
            repos[0]
        );
        assert!(decoded.repos[0].parent.is_none());
    }

    /// The `parent` field carries both the recorded path and the per-repo
    /// parent tip, and round-trips cleanly.
    #[test]
    fn status_json_parent_field_round_trips() {
        let envelope = StatusJsonOutput {
            schema_url: STATUS_SCHEMA_URL.to_string(),
            repos: vec![RepoStatus {
                path: "github/org/repo".into(),
                branch: Some("app--wwb/app--wwa/main".into()),
                tip: Some("deadbeef".into()),
                lock_sha: Some("deadbeef".into()),
                relation: LockRelation::Ok,
                mid_op: None,
                role: "owned".into(),
                url: "https://example.com/repo.git".into(),
                project: "demo".into(),
                absolute_path: "/abs/github/org/repo".into(),
                parent: Some(ParentInfo {
                    // A STACKED parent path: the recorded parent is itself a
                    // workweave, NOT primary. basename(parent)/main would be
                    // wrong here — the field pins the real path instead.
                    path: "/abs/.workweaves/demo--wwa".into(),
                    tip: Some("cafef00d".into()),
                }),
            }],
            resolution: None,
        };

        let json = serde_json::to_string(&envelope).expect("serializes");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parses");
        assert_eq!(
            v["repos"][0]["parent"]["path"],
            "/abs/.workweaves/demo--wwa"
        );
        assert_eq!(v["repos"][0]["parent"]["tip"], "cafef00d");

        let decoded: StatusJsonOutput = serde_json::from_str(&json).expect("deserializes");
        let parent = decoded.repos[0]
            .parent
            .as_ref()
            .expect("parent present after round-trip");
        assert_eq!(parent.path, "/abs/.workweaves/demo--wwa");
        assert_eq!(parent.tip.as_deref(), Some("cafef00d"));
    }
}
