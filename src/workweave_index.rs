//! `projects/<name>/.rwv-workweave-index` — recorded workweave placement and discovery.
//!
//! The registry inverts the workweave marker: each workweave's `.rwv-workweave`
//! marker records `(primary, project, parent)` — telling the workweave where
//! the primary is. The primary-side index records the reverse — for a given
//! `(primary, project)`, where its workweaves live on disk, plus the container
//! directory `workweave create` places new workweaves under by default.
//!
//! ## Location
//!
//! Canonical copy at the primary's project checkout only:
//!
//! ```text
//! <primary>/projects/<project>/.rwv-workweave-index
//! ```
//!
//! Dotted per the machine-local convention (`.rwv-active`, `.rwv-workweave`).
//! Named `-index` to stay more than one character from the `.rwv-workweave`
//! marker: a singular/plural pair would be a confusability trap.
//!
//! ## Format
//!
//! Machine-written JSON via serde. Format chosen per the format-by-audience
//! convention (JSON: simple, has an atomic-write model, well-served by
//! `serde_json`). Reads route to the primary; workweave-side copies (which can
//! only arise from someone committing the file) are never consulted.
//!
//! ```json
//! {
//!   "container": "/abs/path/to/.workweaves",
//!   "workweaves": {
//!     "hotfix": "/abs/path/to/.workweaves/myproj--hotfix"
//!   }
//! }
//! ```
//!
//! ## Advisory, validated before use
//!
//! The index is an advisory inverted index. Every consumer that resolves an
//! entry validates the recorded path carries a `.rwv-workweave` marker whose
//! `primary` canonicalizes to this primary and whose `project` matches. A
//! foreign or stale registry degrades to doctor findings (prune / adopt /
//! flag-tracked), never to acting on wrong paths.
//!
//! Destructive ops hard-require the marker round-trip before touching the
//! directory — a foreign registry cannot direct a deletion at the wrong tree.
//!
//! ## Atomic writes
//!
//! Two `workweave create` invocations from sibling workweaves race on the
//! primary's shared index. `write` uses temp+rename so a concurrent writer
//! never sees a half-written file. Read-modify-write is not lock-serialised
//! (rwv has no daemon); the last writer wins for its whole snapshot. Callers
//! that need read-modify-atomicity (e.g. registering a new workweave) use the
//! `record_workweave` helper, which re-reads immediately before writing so a
//! late-losing race at worst drops an entry another writer already recorded —
//! doctor's container scan re-adopts it.

use crate::manifest::ProjectName;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `.rwv-workweave-index` file name (dotted, machine-local).
///
/// Kept as a constant so the ignore-hygiene layer and doctor's tracked-index
/// scan can reference the same string.
pub const INDEX_FILENAME: &str = ".rwv-workweave-index";

/// The recorded workweave registry for one `(primary, project)` pair.
///
/// `container` — where `workweave create` places new workweaves for this
/// project when no per-workweave override is passed. Absolute path.
///
/// `workweaves` — the recorded name → absolute-path index. The path is the
/// full workweave directory (e.g. `<container>/<project>--<name>`), stored
/// absolute so that per-workweave placement overrides (a `--dir` on create)
/// remain resolvable without re-consulting the container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkweaveIndex {
    /// Absolute container directory for new workweaves in this project.
    pub container: PathBuf,
    /// Recorded `name → absolute path` entries.
    #[serde(default)]
    pub workweaves: BTreeMap<String, PathBuf>,
}

impl WorkweaveIndex {
    /// Construct an empty index with `container` as the recorded container.
    pub fn new(container: PathBuf) -> Self {
        Self {
            container,
            workweaves: BTreeMap::new(),
        }
    }
}

/// The absolute path of the index file for `(primary_root, project)`.
pub fn index_path(primary_root: &Path, project: &ProjectName) -> PathBuf {
    primary_root
        .join("projects")
        .join(project.as_str())
        .join(INDEX_FILENAME)
}

/// The default container for a primary workspace: `<parent-of-root>/.workweaves`.
///
/// This is what `workweave create` records into a fresh index when no other
/// container has been set. Callers should not use this directly for RESOLUTION —
/// go through [`resolve_container`] instead so the recorded container wins.
pub fn default_container(primary_root: &Path) -> PathBuf {
    primary_root
        .parent()
        .expect("workspace root should have a parent")
        .join(".workweaves")
}

/// Read the index file for `(primary_root, project)`.
///
/// Returns `Ok(None)` if the file does not exist (bootstrap case: workspace
/// existed before the index was introduced, or no workweave has been created
/// yet). Callers treat `None` as "empty registry, default container" without
/// silently adopting any on-disk workweaves — adoption is doctor's job.
pub fn read(primary_root: &Path, project: &ProjectName) -> anyhow::Result<Option<WorkweaveIndex>> {
    let path = index_path(primary_root, project);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let index: WorkweaveIndex = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(index))
}

/// Atomically write `index` for `(primary_root, project)`.
///
/// Writes to `<path>.tmp.<pid>` in the same directory then renames over the
/// target — `rename(2)` is atomic within a filesystem, so a concurrent writer
/// or reader never observes a half-written file. If `projects/<project>/`
/// does not exist yet the write fails with an actionable error (rather than
/// silently succeeding into an unowned parent).
pub fn write(
    primary_root: &Path,
    project: &ProjectName,
    index: &WorkweaveIndex,
) -> anyhow::Result<()> {
    let path = index_path(primary_root, project);
    let parent = path
        .parent()
        .expect("index_path always has a parent (projects/<name>/)");
    if !parent.exists() {
        anyhow::bail!(
            "cannot write workweave index: project directory {} does not exist",
            parent.display()
        );
    }
    let content =
        serde_json::to_string_pretty(index).context("failed to serialize workweave index")?;
    let tmp_name = format!("{}.tmp.{}", INDEX_FILENAME, std::process::id());
    let tmp_path = parent.join(&tmp_name);
    std::fs::write(&tmp_path, &content)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

/// Resolve the effective container for new workweaves in `(primary_root, project)`.
///
/// Priority:
///   1. The `container` field of the recorded index, if the index exists.
///   2. The `RWV_WORKWEAVE_DIR` env var, if set (deprecation warning fires
///      via the caller — this function is pure to keep it usable in tests).
///   3. [`default_container`] (`<parent-of-root>/.workweaves`).
///
/// The env var is a transitional fallback; see [`crate::workweave`] for the
/// deprecation-warning path. Once the follow-up bead removes env-var handling
/// entirely, priority (2) drops.
pub fn resolve_container(primary_root: &Path, project: &ProjectName) -> anyhow::Result<PathBuf> {
    if let Some(idx) = read(primary_root, project)? {
        return Ok(idx.container);
    }
    if let Ok(v) = std::env::var("RWV_WORKWEAVE_DIR") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    Ok(default_container(primary_root))
}

/// Set the container in the index for `(primary_root, project)`, creating
/// the index file with an empty `workweaves` map if it did not exist.
///
/// The recorded entries are preserved. `container` should be an absolute
/// path; the caller canonicalizes if needed.
pub fn set_container(
    primary_root: &Path,
    project: &ProjectName,
    container: PathBuf,
) -> anyhow::Result<()> {
    let mut index =
        read(primary_root, project)?.unwrap_or_else(|| WorkweaveIndex::new(container.clone()));
    index.container = container;
    write(primary_root, project, &index)
}

/// Record a workweave entry `name → path` in the index for
/// `(primary_root, project)`, creating the index if it does not exist.
///
/// Read-modify-write with atomic rename. Concurrent writers may race on
/// this: the last writer wins with its whole snapshot. A losing writer's
/// entry then goes missing until doctor's container-scoped scan re-adopts
/// the on-disk workweave into the registry — which is exactly the reconcile
/// path the design describes.
///
/// `default_container` is used to seed the container when the index does
/// not yet exist and no override is available.
pub fn record_workweave(
    primary_root: &Path,
    project: &ProjectName,
    name: &str,
    path: PathBuf,
) -> anyhow::Result<()> {
    let mut index = match read(primary_root, project)? {
        Some(idx) => idx,
        None => {
            // Bootstrap: seed with the effective container so the next
            // create's default lands in the same place. Prefer the env var
            // fallback (transitional) then the compiled-in default.
            let seed = if let Ok(v) = std::env::var("RWV_WORKWEAVE_DIR") {
                if !v.is_empty() {
                    PathBuf::from(v)
                } else {
                    default_container(primary_root)
                }
            } else {
                default_container(primary_root)
            };
            WorkweaveIndex::new(seed)
        }
    };
    index.workweaves.insert(name.to_string(), path);
    write(primary_root, project, &index)
}

/// Remove a workweave entry from the index. No-op if the entry (or the index
/// file) does not exist.
///
/// Idempotent: a delete that races with another writer's insert may leave
/// the entry, but doctor will prune it on the next round (marker
/// round-trip against the missing directory).
pub fn forget_workweave(
    primary_root: &Path,
    project: &ProjectName,
    name: &str,
) -> anyhow::Result<()> {
    let mut index = match read(primary_root, project)? {
        Some(idx) => idx,
        None => return Ok(()),
    };
    if index.workweaves.remove(name).is_none() {
        return Ok(());
    }
    write(primary_root, project, &index)
}

/// Look up the recorded path for a workweave without any marker validation.
///
/// Callers that consume the path (list rendering, destructive ops) MUST
/// validate the marker round-trip via [`crate::workweave::validate_registry_entry`]
/// before acting on the path. This helper is a raw registry read; validation
/// is a separate step so tests can exercise the invalid-entry paths.
pub fn lookup_raw(
    primary_root: &Path,
    project: &ProjectName,
    name: &str,
) -> anyhow::Result<Option<PathBuf>> {
    Ok(read(primary_root, project)?.and_then(|idx| idx.workweaves.get(name).cloned()))
}

/// Ensure the project repo's ignore-surface excludes `.rwv-workweave-index`.
///
/// Hygiene, not correctness: the design tolerates a committed copy (reads
/// route to the primary; doctor flags a tracked index as a finding).
///
/// Two candidate targets, prioritised for zero shared-repo footprint:
///
/// 1. `.git/info/exclude` — per-clone, invisible, never touches the
///    working tree, so it does not perturb any dirty-tree check running
///    concurrently. This is what we write when the project is a git repo.
/// 2. `.gitignore` — VCS-equivalent for non-git project repos (recorded
///    fallback). Committed alongside the project, at the cost of adding
///    an rwv-specific entry the operator has to accept.
///
/// The design mentions both options as acceptable. We pick option 1 by
/// default (git-managed projects) so that a freshly-created workweave
/// never turns the primary project into a dirty tree — sync-to and the
/// dirty-check-then-refuse precondition matter more than the `.gitignore`
/// entry being self-documenting.
///
/// Best effort: silently succeeds on any I/O error. Doctor's
/// `tracked-index` finding is the correctness net if a committed copy
/// slips through.
pub fn ensure_ignore_entry(primary_root: &Path, project: &ProjectName) -> anyhow::Result<()> {
    let project_dir = primary_root.join("projects").join(project.as_str());
    if !project_dir.exists() {
        // Not our failure — creation flows precede us.
        return Ok(());
    }
    // Prefer `.git/info/exclude` when the project is a git repo (works for
    // both a plain repo `.git/` directory and a submodule `.git` file that
    // points to a gitdir elsewhere).
    if let Some(git_info_dir) = git_info_dir(&project_dir) {
        let exclude = git_info_dir.join("exclude");
        return append_ignore_line(&exclude);
    }
    // Fall back to a committed `.gitignore` for non-git project repos.
    let gitignore = project_dir.join(".gitignore");
    append_ignore_line(&gitignore)
}

/// Resolve `<project_dir>/.git/info/` for a project that is either a
/// plain-`.git`-dir clone or a linked worktree (`.git` file pointing at
/// the actual gitdir).
///
/// Returns `None` when the project is not a git-managed repo.
fn git_info_dir(project_dir: &Path) -> Option<PathBuf> {
    let git_entry = project_dir.join(".git");
    if git_entry.is_dir() {
        let info = git_entry.join("info");
        std::fs::create_dir_all(&info).ok()?;
        return Some(info);
    }
    if git_entry.is_file() {
        let content = std::fs::read_to_string(&git_entry).ok()?;
        // Format: `gitdir: <path>` (possibly relative).
        let stripped = content.trim().strip_prefix("gitdir:")?.trim();
        let gitdir = PathBuf::from(stripped);
        let gitdir = if gitdir.is_absolute() {
            gitdir
        } else {
            project_dir.join(gitdir)
        };
        // For linked worktrees the `info/` we want to touch is the
        // COMMON info dir, not the worktree-specific one.
        let common = gitdir.join("commondir");
        let common_target = if common.exists() {
            let s = std::fs::read_to_string(&common).ok()?;
            let rel = PathBuf::from(s.trim());
            if rel.is_absolute() {
                rel
            } else {
                gitdir.join(rel)
            }
        } else {
            gitdir
        };
        let info = common_target.join("info");
        std::fs::create_dir_all(&info).ok()?;
        return Some(info);
    }
    None
}

/// Append `INDEX_FILENAME` to `target` if it is not already present.
fn append_ignore_line(target: &Path) -> anyhow::Result<()> {
    let existing = std::fs::read_to_string(target).unwrap_or_default();
    let needle = INDEX_FILENAME;
    let already_present = existing
        .lines()
        .map(str::trim)
        .any(|line| line == needle || line == format!("/{needle}"));
    if already_present {
        return Ok(());
    }
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(needle);
    new_content.push('\n');
    // Ensure parent (for `.git/info/`) exists — best-effort; append_ignore_line
    // may be called with a `.gitignore` in `projects/<name>/` which always
    // has its parent present.
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(target, new_content)
        .with_context(|| format!("failed to update {}", target.display()))?;
    Ok(())
}

/// Enumerate every project directly under `<primary_root>/projects/`.
///
/// A helper for callers that need to iterate every project's registry
/// (e.g. adopting children across the workspace when a workweave is
/// retired). Returns projects sorted by name; directories missing an
/// `rwv.yaml` are still included (a project can register workweaves before
/// its manifest is populated).
pub fn projects_on_disk(primary_root: &Path) -> Vec<ProjectName> {
    let projects_dir = primary_root.join("projects");
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<ProjectName> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .map(ProjectName::new)
        .collect();
    names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    names
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_project(primary: &Path, name: &str) -> ProjectName {
        let p = primary.join("projects").join(name);
        std::fs::create_dir_all(&p).unwrap();
        ProjectName::new(name)
    }

    #[test]
    fn read_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let got = read(&primary, &project).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn write_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let mut index = WorkweaveIndex::new(PathBuf::from("/abs/container"));
        index.workweaves.insert(
            "hotfix".to_string(),
            PathBuf::from("/abs/container/web-app--hotfix"),
        );
        write(&primary, &project, &index).unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert_eq!(got.container, PathBuf::from("/abs/container"));
        assert_eq!(
            got.workweaves.get("hotfix").unwrap(),
            &PathBuf::from("/abs/container/web-app--hotfix")
        );
    }

    #[test]
    fn record_workweave_seeds_index_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(
            &primary,
            &project,
            "feat-a",
            PathBuf::from("/abs/container/web-app--feat-a"),
        )
        .unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        // Container defaults to `<parent-of-root>/.workweaves` when no env var set.
        assert_eq!(got.container, primary.parent().unwrap().join(".workweaves"));
        assert_eq!(got.workweaves.len(), 1);
    }

    #[test]
    fn forget_workweave_removes_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(&primary, &project, "a", PathBuf::from("/x/web-app--a")).unwrap();
        record_workweave(&primary, &project, "b", PathBuf::from("/x/web-app--b")).unwrap();
        forget_workweave(&primary, &project, "a").unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert!(!got.workweaves.contains_key("a"));
        assert!(got.workweaves.contains_key("b"));
    }

    #[test]
    fn forget_workweave_noop_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        // No index file; must not error.
        forget_workweave(&primary, &project, "nonexistent").unwrap();
    }

    #[test]
    fn set_container_preserves_existing_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(&primary, &project, "a", PathBuf::from("/orig/web-app--a")).unwrap();
        set_container(&primary, &project, PathBuf::from("/new-container")).unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert_eq!(got.container, PathBuf::from("/new-container"));
        assert_eq!(
            got.workweaves.get("a").unwrap(),
            &PathBuf::from("/orig/web-app--a")
        );
    }

    #[test]
    fn ensure_ignore_entry_creates_gitignore_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        ensure_ignore_entry(&primary, &project).unwrap();
        let content = std::fs::read_to_string(primary.join("projects/web-app/.gitignore")).unwrap();
        assert!(content.contains(INDEX_FILENAME));
    }

    #[test]
    fn ensure_ignore_entry_idempotent_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let gitignore = primary.join("projects/web-app/.gitignore");
        std::fs::write(&gitignore, "target/\n.rwv-workweave-index\nnode_modules/\n").unwrap();
        ensure_ignore_entry(&primary, &project).unwrap();
        let content = std::fs::read_to_string(&gitignore).unwrap();
        // Line count should be unchanged (3 non-empty lines).
        let occurrences = content.matches(INDEX_FILENAME).count();
        assert_eq!(occurrences, 1, "must not duplicate the ignore entry");
    }

    #[test]
    fn write_fails_without_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        // Note: no projects/web-app dir.
        let project = ProjectName::new("web-app");

        let index = WorkweaveIndex::new(PathBuf::from("/x"));
        let result = write(&primary, &project, &index);
        assert!(result.is_err(), "write must fail without project dir");
    }

    #[test]
    fn resolve_container_prefers_recorded_over_default() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        set_container(&primary, &project, PathBuf::from("/recorded")).unwrap();
        let got = resolve_container(&primary, &project).unwrap();
        assert_eq!(got, PathBuf::from("/recorded"));
    }

    #[test]
    fn resolve_container_falls_back_to_default_when_no_index_and_no_env() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        // Clear the env var for this test — global state, so serialize with
        // other env-touching tests only if this becomes a problem.
        // Safe: we save and restore.
        let prev = std::env::var("RWV_WORKWEAVE_DIR").ok();
        std::env::remove_var("RWV_WORKWEAVE_DIR");

        let got = resolve_container(&primary, &project).unwrap();
        assert_eq!(got, primary.parent().unwrap().join(".workweaves"));

        if let Some(v) = prev {
            std::env::set_var("RWV_WORKWEAVE_DIR", v);
        }
    }
}
