//! `rwv fetch` — clone a project and its repos into the workspace.
//!
//! The source must be a URL (full URL or `owner/repo` shorthand resolved via
//! the registry). Local paths are not accepted; use `rwv activate` instead.

use crate::git::{git_command, GitVcs};
use crate::lock;
use crate::manifest::{clone_urls_equivalent, LockFile, Manifest, RepoPath, Role};
use crate::registry;
use crate::vcs::Vcs;
use anyhow::{bail, Context};
use std::path::Path;

/// Controls how `rwv fetch` resolves repo versions.
///
/// - `Default`: read `rwv.lock` and align clones to it. The lock is the
///   source of truth for which revision each repo should be at. When the
///   lock is absent, fetch bootstraps it from branch HEAD (one-time event).
///   When a manifest entry is missing from the lock, it is added at branch
///   HEAD (additive only — never moves existing SHAs).
/// - `Frozen`: like `Default`, but errors if the lock file is missing or
///   does not cover all manifest repos (CI mode). Never mutates the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMode {
    /// Read rwv.lock, align clones, bootstrap or add missing entries
    /// additively. Auto-activates the project on first fetch.
    Default,
    /// Like Default, but error if the lock would change at all
    /// (including missing entries). No bootstrap.
    Frozen,
}

/// Derive a project name from a source URL or path.
///
/// Takes the last path segment and strips a trailing `.git` suffix.
pub fn project_name_from_source(source: &str) -> String {
    // Strip trailing slashes, then take the last segment.
    let trimmed = source.trim_end_matches('/');
    let last_segment = trimmed.rsplit('/').next().unwrap_or(trimmed);
    // Also handle git@host:owner/repo.git — take after last ':'
    let last_segment = last_segment.rsplit(':').next().unwrap_or(last_segment);
    last_segment
        .strip_suffix(".git")
        .unwrap_or(last_segment)
        .to_string()
}

/// Resolve `source` to a clone URL and the owner string.
///
/// Accepts full URLs (returned as-is) or `owner/repo` / `registry/owner/repo`
/// shorthand (resolved via the built-in registries to an HTTPS clone URL).
///
/// Returns `(url, owner)` where `owner` may be empty for unrecognised URLs.
fn resolve_source(source: &str) -> anyhow::Result<(crate::manifest::RepoUrl, String)> {
    let parsed: crate::manifest::RepoUrl = source.parse()?;
    let info = registry::resolve_to_clone_info(&parsed)?;
    let owner = info.id.owner().to_owned();
    Ok((info.url, owner))
}

/// Validate that a lock file covers all repos in the manifest.
///
/// Returns a list of repo paths present in the manifest but missing from the lock.
/// When `no_reference` is set, `reference`-role repos are excluded from the
/// check — the user has opted out of fetching them, so missing lock entries
/// for them shouldn't fail `--frozen`.
fn find_stale_repos(manifest: &Manifest, lock: &LockFile, no_reference: bool) -> Vec<RepoPath> {
    manifest
        .repositories
        .iter()
        .filter(|(_, entry)| !(no_reference && entry.role == Role::Reference))
        .map(|(rp, _)| rp)
        .filter(|rp| !lock.repositories.contains_key(*rp))
        .cloned()
        .collect()
}

/// Run the fetch command: clone a project source, then align repos to the
/// lock file (bootstrapping it if necessary in Default mode).
///
/// `workspace_root` is the directory where repos and `projects/` live (CWD).
///
/// Lock mutation:
/// - `Default`: bootstrap the lock from branch HEAD when absent; otherwise
///   read existing entries and additively add missing ones at branch HEAD.
///   Never advances entries that already exist in the lock.
/// - `Frozen`: never writes the lock; errors if missing or stale (CI mode).
pub fn run_fetch(
    source: &str,
    workspace_root: &Path,
    mode: FetchMode,
    no_reference: bool,
) -> anyhow::Result<()> {
    let git = GitVcs;

    // Resolve source to a clone URL (supports full URLs and owner/repo shorthand).
    let (url, owner) = resolve_source(source)?;
    let url_str = url.to_string();
    let name = project_name_from_source(&url_str);
    let projects_dir = workspace_root.join("projects");
    std::fs::create_dir_all(&projects_dir).context("failed to create projects/ directory")?;
    let project_dir = projects_dir.join(&name);
    if project_dir.exists() {
        // Project name already taken — surface a helpful scoped-path hint.
        let scoped = if owner.is_empty() {
            format!("projects/{{owner}}/{name}/")
        } else {
            format!("projects/{owner}/{name}/")
        };
        eprintln!("Error: project '{name}' already exists at projects/{name}/");
        eprintln!("Hint: try a scoped path: {scoped}");
        bail!("project '{}' already exists at projects/{}/", name, name);
    } else {
        println!("rwv fetch: cloning project '{}'", name);
        git.clone_repo(&url_str, &project_dir)
            .with_context(|| format!("failed to clone project source '{}'", url))?;
    }

    // Read the manifest
    let manifest_path = project_dir.join("rwv.yaml");
    let manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to read manifest from {}", manifest_path.display()))?;

    // Load the lock file. In Frozen mode the lock must exist and cover all
    // manifest repos. In Default mode the lock may be absent (bootstrap), and
    // missing repos are added additively after fetch.
    let lock_path = project_dir.join("rwv.lock");
    let existing_lock: Option<LockFile> = if lock_path.exists() {
        Some(
            LockFile::from_path(&lock_path)
                .with_context(|| format!("failed to read lock file at {}", lock_path.display()))?,
        )
    } else {
        None
    };

    match mode {
        FetchMode::Frozen => {
            let lock = existing_lock.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "rwv fetch --frozen: lock file does not exist at {}",
                    lock_path.display()
                )
            })?;
            let missing = find_stale_repos(&manifest, lock, no_reference);
            if !missing.is_empty() {
                let names: Vec<&str> = missing.iter().map(|rp| rp.as_str()).collect();
                bail!(
                    "rwv fetch --frozen: lock file is stale; repos not covered by lock: {}",
                    names.join(", ")
                );
            }
        }
        FetchMode::Default => {
            // Lock-not-present is a normal bootstrap. Missing entries are
            // additive and dealt with below.
        }
    }

    // Warn about orphan lock entries (in lock, not in manifest). Doesn't fail
    // and doesn't touch the clones.
    if let Some(ref lock) = existing_lock {
        for repo_path in lock.repositories.keys() {
            if !manifest.repositories.contains_key(repo_path) {
                eprintln!(
                    "rwv fetch: warning: orphan in lock: {} (lock entry has no manifest entry)",
                    repo_path.as_str()
                );
            }
        }
    }

    // Clone each repo to its canonical path, collecting errors so that one
    // failure does not prevent the remaining repos from being attempted.
    let mut succeeded = 0usize;
    let mut errors: Vec<String> = Vec::new();
    // Repos that were added to (or bootstrapped into) the lock during this
    // fetch — used to drive the Default-mode lock-write step at the end.
    let mut added_to_lock: Vec<RepoPath> = Vec::new();

    for (repo_path, entry) in &manifest.repositories {
        if no_reference && entry.role == Role::Reference {
            println!(
                "rwv fetch: skipping {} (role: reference)",
                repo_path.as_str()
            );
            continue;
        }
        let dest = workspace_root.join(repo_path.as_path());

        // Look up the corresponding lock entry, if any.
        let lock_entry = existing_lock
            .as_ref()
            .and_then(|l| l.repositories.get(repo_path).cloned());

        if dest.exists() {
            // If the existing clone is role=fork and its `origin` still points
            // at the source-of-record, warn the user — `git push` would target
            // the upstream and get 403'd. We leave remotes alone.
            if entry.role == Role::Fork {
                maybe_warn_fork_origin(&dest, repo_path.as_str(), &entry.url.to_string());
            }
            if let Some(lock_entry) = lock_entry {
                println!(
                    "rwv fetch: checking out {} at {}",
                    repo_path.as_str(),
                    lock_entry.version,
                );
                let resolved = match git.resolve_revision(&dest, lock_entry.version.as_str()) {
                    Ok(r) => r,
                    Err(e) => {
                        let msg = format!(
                            "{}: failed to resolve {}: {e}",
                            repo_path.as_str(),
                            lock_entry.version,
                        );
                        eprintln!("rwv fetch: error: {msg}");
                        errors.push(msg);
                        continue;
                    }
                };
                if let Err(e) = git.checkout(&dest, &resolved) {
                    let msg = format!(
                        "{}: failed to check out {}: {e}",
                        repo_path.as_str(),
                        lock_entry.version,
                    );
                    eprintln!("rwv fetch: error: {msg}");
                    errors.push(msg);
                    continue;
                }
            } else if existing_lock.is_some() {
                // Lock exists but doesn't cover this repo — additive add at
                // branch HEAD. The clone already exists; nothing to do
                // beyond marking it for the lock write below.
                println!(
                    "rwv fetch: adding {} to lock at branch HEAD (additive)",
                    repo_path.as_str()
                );
                added_to_lock.push(repo_path.clone());
            } else {
                // Bootstrap (no lock yet) — clone is pre-existing, just
                // record it. The lock-write step below will snapshot
                // everything from disk.
                println!("rwv fetch: skip {} (already exists)", repo_path.as_str());
            }
            succeeded += 1;
            continue;
        }

        // Create parent directories
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let msg = format!(
                    "{}: failed to create directory {}: {e}",
                    repo_path.as_str(),
                    parent.display()
                );
                eprintln!("rwv fetch: error: {msg}");
                errors.push(msg);
                continue;
            }
        }

        println!(
            "rwv fetch: cloning {} from {} (role: {})",
            repo_path.as_str(),
            entry.url,
            entry.role.as_str()
        );
        if let Err(e) = git.clone_with_role(&entry.url.to_string(), &dest, entry.role) {
            let msg = format!(
                "{}: failed to clone {} into {}: {e}",
                repo_path.as_str(),
                entry.url,
                dest.display()
            );
            eprintln!("rwv fetch: error: {msg}");
            errors.push(msg);
            continue;
        }

        // After clone, check out the lock-pinned revision when one exists.
        if let Some(lock_entry) = lock_entry {
            println!(
                "rwv fetch: checking out {} at {}",
                repo_path.as_str(),
                lock_entry.version,
            );
            let resolved = match git.resolve_revision(&dest, lock_entry.version.as_str()) {
                Ok(r) => r,
                Err(e) => {
                    let msg = format!(
                        "{}: failed to resolve {}: {e}",
                        repo_path.as_str(),
                        lock_entry.version,
                    );
                    eprintln!("rwv fetch: error: {msg}");
                    errors.push(msg);
                    continue;
                }
            };
            if let Err(e) = git.checkout(&dest, &resolved) {
                let msg = format!(
                    "{}: failed to check out {}: {e}",
                    repo_path.as_str(),
                    lock_entry.version,
                );
                eprintln!("rwv fetch: error: {msg}");
                errors.push(msg);
                continue;
            }
        } else if existing_lock.is_some() {
            // Lock exists but doesn't cover this repo — leave at branch HEAD
            // (where the clone landed) and mark for additive lock entry.
            added_to_lock.push(repo_path.clone());
        }
        // else: bootstrap, will be picked up wholesale below.

        succeeded += 1;
    }

    // Summary
    let total = succeeded + errors.len();
    if !errors.is_empty() {
        eprintln!(
            "rwv fetch: {succeeded}/{total} repo(s) succeeded, {} failed:",
            errors.len()
        );
        for msg in &errors {
            eprintln!("  - {msg}");
        }
        bail!(
            "fetch completed with {} clone failure(s) out of {total} repo(s)",
            errors.len()
        )
    }

    println!("rwv fetch: done ({succeeded} repo(s) ready)");

    // Default mode: bootstrap or additively extend the lock; then maybe auto-activate.
    if mode == FetchMode::Default {
        let needs_bootstrap = existing_lock.is_none();
        let has_additions = !added_to_lock.is_empty();

        // generate_lock walks every entry in the manifest and runs
        // `git rev-parse HEAD` against each on-disk repo. When --no-reference
        // is set, reference repos were skipped above and their directories
        // don't exist on disk — drop them from the manifest used for lock
        // generation so we don't trip on the missing paths.
        let lock_manifest = if no_reference {
            let mut filtered = manifest.clone();
            filtered
                .repositories
                .retain(|_, entry| entry.role != Role::Reference);
            std::borrow::Cow::Owned(filtered)
        } else {
            std::borrow::Cow::Borrowed(&manifest)
        };

        if needs_bootstrap {
            // Snapshot the full set of manifest repos from disk.
            let new_lock = lock::generate_lock(&lock_manifest, workspace_root, None, true)?;
            lock::write_lock(&new_lock, &lock_path)?;
            eprintln!("rwv fetch: wrote {}", lock_path.display());
        } else if has_additions {
            // Preserve existing lock entries as written (do not re-resolve
            // — that could rewrite tag-form versions as raw SHAs). Append
            // new entries by snapshotting HEAD for the added repos.
            let mut merged = existing_lock
                .as_ref()
                .expect("existing_lock is Some when !needs_bootstrap")
                .clone();
            // Generate a fresh lock for new entries only.
            let new_lock = lock::generate_lock(&lock_manifest, workspace_root, None, true)?;
            for repo_path in &added_to_lock {
                if let Some(entry) = new_lock.repositories.get(repo_path) {
                    // Convert ResolvedLockEntry to LockEntry for the merge
                    // (canonical SHA serializes the same as raw scalar).
                    let raw_entry = crate::manifest::LockEntry {
                        vcs_type: entry.vcs_type,
                        url: entry.url.clone(),
                        version: crate::vcs::RawRevisionId::new(entry.version.display_str()),
                    };
                    merged.repositories.insert(repo_path.clone(), raw_entry);
                }
            }
            lock::write_lock(&merged, &lock_path)?;
            eprintln!("rwv fetch: wrote {}", lock_path.display());
        }

        // Auto-activate only when no project is already active (first fetch).
        let active_file = workspace_root.join(".rwv-active");
        if active_file.exists() {
            println!(
                "rwv fetch: skipping auto-activate (project '{}' already active)",
                std::fs::read_to_string(&active_file)
                    .unwrap_or_default()
                    .trim()
            );
        } else {
            crate::activate::activate(&name, workspace_root)?;
        }
    }

    Ok(())
}

/// If `dest` is a git repo with `origin` pointing at `manifest_url`, print a
/// short stderr notice telling the user to rename the remote to `upstream`.
/// Non-fatal: silent on any git error or when remotes differ.
pub(crate) fn maybe_warn_fork_origin(dest: &Path, repo_label: &str, manifest_url: &str) {
    let out = match git_command()
        .args(["remote", "get-url", "origin"])
        .current_dir(dest)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return,
    };
    let origin_url = match String::from_utf8(out.stdout) {
        Ok(s) => s.trim().to_owned(),
        Err(_) => return,
    };
    if clone_urls_equivalent(&origin_url, manifest_url) {
        eprintln!(
            "note: {repo_label} is role=fork but origin points at the source-of-record; \
rename with `git remote rename origin upstream` to avoid pushing there by accident"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_from_https_url() {
        assert_eq!(
            project_name_from_source("https://github.com/org/myproject.git"),
            "myproject"
        );
    }

    #[test]
    fn project_name_from_https_url_no_git_suffix() {
        assert_eq!(
            project_name_from_source("https://github.com/org/myproject"),
            "myproject"
        );
    }

    #[test]
    fn project_name_from_file_url() {
        assert_eq!(
            project_name_from_source("file:///tmp/project.git"),
            "project"
        );
    }

    #[test]
    fn project_name_from_file_url_trailing_slash() {
        assert_eq!(
            project_name_from_source("file:///tmp/project.git/"),
            "project"
        );
    }

    #[test]
    fn project_name_from_ssh_url() {
        assert_eq!(
            project_name_from_source("git@github.com:org/repo.git"),
            "repo"
        );
    }

    #[test]
    fn project_name_from_plain_name() {
        assert_eq!(project_name_from_source("my-project"), "my-project");
    }

    #[test]
    fn resolve_source_passes_through_urls() {
        let url = "https://github.com/org/repo.git";
        let (resolved_url, owner) = resolve_source(url).unwrap();
        assert_eq!(resolved_url.to_string(), url);
        assert_eq!(owner, "org");
    }

    #[test]
    fn resolve_source_passes_through_ssh_urls() {
        let url = "git@github.com:org/repo.git";
        let (resolved_url, owner) = resolve_source(url).unwrap();
        assert_eq!(resolved_url.to_string(), url);
        assert_eq!(owner, "org");
    }

    #[test]
    fn resolve_source_passes_through_file_urls() {
        let url = "file:///tmp/repo.git";
        let (resolved_url, owner) = resolve_source(url).unwrap();
        assert_eq!(resolved_url.to_string(), url);
        // file:// URLs that don't match any registry have an empty owner
        let _ = owner; // owner may be empty or a path segment; just verify no panic
    }

    #[test]
    fn resolve_source_resolves_two_part_shorthand() {
        let (url, owner) = resolve_source("cwalv/repoweave").unwrap();
        assert_eq!(url.to_string(), "https://github.com/cwalv/repoweave.git");
        assert_eq!(owner, "cwalv");
    }

    #[test]
    fn resolve_source_resolves_three_part_shorthand() {
        let (url, owner) = resolve_source("gitlab/org/proj").unwrap();
        assert_eq!(url.to_string(), "https://gitlab.com/org/proj.git");
        assert_eq!(owner, "org");
    }

    #[test]
    fn resolve_source_rejects_invalid_shorthand() {
        assert!(resolve_source("not-a-valid-source").is_err());
    }

    #[test]
    fn resolve_source_rejects_four_part_shorthand() {
        assert!(resolve_source("a/b/c/d").is_err());
    }

    // find_stale_repos: --no-reference should exempt reference repos

    fn make_entry(role: Role) -> crate::manifest::RepoEntry {
        crate::manifest::RepoEntry {
            vcs_type: crate::manifest::VcsType::Git,
            url: "https://example.com/repo.git".parse().unwrap(),
            version: crate::vcs::RefName::new("main"),
            role,
        }
    }

    fn make_lock_entry() -> crate::manifest::LockEntry {
        crate::manifest::LockEntry {
            vcs_type: crate::manifest::VcsType::Git,
            url: "https://example.com/repo.git".parse().unwrap(),
            version: crate::vcs::RawRevisionId::new("abc123"),
        }
    }

    #[test]
    fn find_stale_repos_flags_reference_when_no_reference_is_false() {
        let mut manifest = Manifest {
            repositories: Default::default(),
            integrations: Default::default(),
            workweave: None,
        };
        let primary = RepoPath::new("github/org/primary");
        let reference = RepoPath::new("github/org/reference");
        manifest
            .repositories
            .insert(primary.clone(), make_entry(Role::Primary));
        manifest
            .repositories
            .insert(reference.clone(), make_entry(Role::Reference));

        // Lock covers only the primary — reference is "stale".
        let mut lock = LockFile {
            workweave: None,
            repositories: Default::default(),
        };
        lock.repositories.insert(primary, make_lock_entry());

        let stale = find_stale_repos(&manifest, &lock, false);
        assert_eq!(stale, vec![reference]);
    }

    #[test]
    fn find_stale_repos_excludes_reference_when_no_reference_is_true() {
        let mut manifest = Manifest {
            repositories: Default::default(),
            integrations: Default::default(),
            workweave: None,
        };
        let primary = RepoPath::new("github/org/primary");
        let reference = RepoPath::new("github/org/reference");
        manifest
            .repositories
            .insert(primary.clone(), make_entry(Role::Primary));
        manifest
            .repositories
            .insert(reference, make_entry(Role::Reference));

        let mut lock = LockFile {
            workweave: None,
            repositories: Default::default(),
        };
        lock.repositories.insert(primary, make_lock_entry());

        // With no_reference=true, the missing reference entry is not flagged.
        let stale = find_stale_repos(&manifest, &lock, true);
        assert!(stale.is_empty(), "expected empty, got {stale:?}");
    }

    // FetchMode enum tests

    #[test]
    fn fetch_mode_variants_are_distinct() {
        assert_ne!(FetchMode::Default, FetchMode::Frozen);
    }

    #[test]
    fn fetch_mode_default_is_default_variant() {
        // The default mode (no flags) should be FetchMode::Default.
        let mode = FetchMode::Default;
        assert_eq!(mode, FetchMode::Default);
    }

    #[test]
    fn fetch_mode_is_copy() {
        // FetchMode should be Copy — it's a simple enum.
        let a = FetchMode::Default;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn fetch_mode_debug() {
        // Verify Debug is derived (used in error messages).
        let s = format!("{:?}", FetchMode::Frozen);
        assert!(s.contains("Frozen"));
    }
}
