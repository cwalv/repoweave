//! `rwv fetch` — clone a project and its repos into the workspace.
//!
//! The source can be a URL (cloned to `projects/{name}/`) or a local path
//! to an existing project directory.

use crate::git::GitVcs;
use crate::lock;
use crate::manifest::{LockFile, Manifest, RepoPath};
use crate::vcs::Vcs;
use anyhow::{bail, Context};
use std::path::Path;

/// Controls how `rwv fetch` resolves repo versions.
///
/// - `Default`: fetch branch HEAD from `rwv.yaml`, then update `rwv.lock`
///   with the resolved SHAs (like `npm install`).
/// - `Locked`: check out each repo at the exact revision in `rwv.lock`
///   (like `npm ci` with a valid lock).
/// - `Frozen`: like `Locked`, but errors if the lock file is missing or
///   does not cover all manifest repos (CI mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMode {
    /// Fetch branch HEAD, update rwv.lock, auto-activate.
    Default,
    /// Check out exact revisions from rwv.lock.
    Locked,
    /// Like Locked, but error on missing or stale lock.
    Frozen,
}

/// Derive a project name from a source URL or path.
///
/// Takes the last path segment and strips a trailing `.git` suffix.
pub fn project_name_from_source(source: &str) -> String {
    // Strip trailing slashes, then take the last segment.
    let trimmed = source.trim_end_matches('/');
    let last_segment = trimmed
        .rsplit('/')
        .next()
        .unwrap_or(trimmed);
    // Also handle git@host:owner/repo.git — take after last ':'
    let last_segment = last_segment
        .rsplit(':')
        .next()
        .unwrap_or(last_segment);
    last_segment
        .strip_suffix(".git")
        .unwrap_or(last_segment)
        .to_string()
}

/// Returns true if `source` looks like a URL (as opposed to a local path).
fn is_url(source: &str) -> bool {
    source.contains("://") || source.starts_with("git@")
}

/// Validate that a lock file covers all repos in the manifest.
///
/// Returns a list of repo paths present in the manifest but missing from the lock.
fn find_stale_repos(manifest: &Manifest, lock: &LockFile) -> Vec<RepoPath> {
    manifest
        .repositories
        .keys()
        .filter(|rp| !lock.repositories.contains_key(*rp))
        .cloned()
        .collect()
}

/// Run the fetch command: clone a project source and all its repos.
///
/// `workspace_root` is the directory where repos and `projects/` live (CWD).
pub fn run_fetch(source: &str, workspace_root: &Path, mode: FetchMode) -> anyhow::Result<()> {
    let git = GitVcs;

    let project_dir = if is_url(source) {
        // Clone the project repo into projects/{name}/
        let name = project_name_from_source(source);
        let projects_dir = workspace_root.join("projects");
        std::fs::create_dir_all(&projects_dir)
            .context("failed to create projects/ directory")?;
        let project_dir = projects_dir.join(&name);
        if project_dir.exists() {
            // Project already exists — re-read its manifest and continue
            // to ensure repos are cloned.
            println!("rwv fetch: project '{}' already exists, skipping clone", name);
        } else {
            println!("rwv fetch: cloning project '{}'", name);
            git.clone_repo(source, &project_dir)
                .with_context(|| format!("failed to clone project source '{}'", source))?;
        }
        project_dir
    } else {
        // Local path — must be an existing directory with rwv.yaml
        let path = Path::new(source);
        if !path.exists() {
            bail!("Error: source path '{}' not found", source);
        }
        if !path.join("rwv.yaml").exists() {
            bail!(
                "Error: source path '{}' does not contain an rwv.yaml manifest",
                source
            );
        }
        path.to_path_buf()
    };

    // Read the manifest
    let manifest_path = project_dir.join("rwv.yaml");
    let manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to read manifest from {}", manifest_path.display()))?;

    // For Locked/Frozen modes, load the lock file.
    let lock_path = project_dir.join("rwv.lock");
    let lock_file = match mode {
        FetchMode::Frozen => {
            if !lock_path.exists() {
                bail!(
                    "rwv fetch --frozen: lock file does not exist at {}",
                    lock_path.display()
                );
            }
            let lock = LockFile::from_path(&lock_path)
                .with_context(|| format!("failed to read lock file at {}", lock_path.display()))?;
            let missing = find_stale_repos(&manifest, &lock);
            if !missing.is_empty() {
                let names: Vec<&str> = missing.iter().map(|rp| rp.as_str()).collect();
                bail!(
                    "rwv fetch --frozen: lock file is stale; repos not covered by lock: {}",
                    names.join(", ")
                );
            }
            Some(lock)
        }
        FetchMode::Locked => {
            if lock_path.exists() {
                Some(
                    LockFile::from_path(&lock_path)
                        .with_context(|| {
                            format!("failed to read lock file at {}", lock_path.display())
                        })?,
                )
            } else {
                None
            }
        }
        FetchMode::Default => None,
    };

    // Clone each repo to its canonical path, collecting errors so that one
    // failure does not prevent the remaining repos from being attempted.
    let mut succeeded = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for (repo_path, entry) in &manifest.repositories {
        let dest = workspace_root.join(repo_path.as_path());
        if dest.exists() {
            // For Locked/Frozen, check out the pinned revision even if already cloned.
            if let Some(ref lock) = lock_file {
                if let Some(lock_entry) = lock.repositories.get(repo_path) {
                    println!(
                        "rwv fetch: checking out {} at {}",
                        repo_path.as_str(),
                        lock_entry.version,
                    );
                    if let Err(e) = git.checkout(&dest, lock_entry.version.as_str()) {
                        let msg = format!(
                            "{}: failed to check out {}: {e}",
                            repo_path.as_str(),
                            lock_entry.version,
                        );
                        eprintln!("rwv fetch: error: {msg}");
                        errors.push(msg);
                        continue;
                    }
                }
            } else {
                println!(
                    "rwv fetch: skip {} (already exists)",
                    repo_path.as_str()
                );
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

        println!("rwv fetch: cloning {} from {}", repo_path.as_str(), entry.url);
        if let Err(e) = git.clone_repo(&entry.url, &dest) {
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

        // For Locked/Frozen, check out the pinned revision after clone.
        if let Some(ref lock) = lock_file {
            if let Some(lock_entry) = lock.repositories.get(repo_path) {
                println!(
                    "rwv fetch: checking out {} at {}",
                    repo_path.as_str(),
                    lock_entry.version,
                );
                if let Err(e) = git.checkout(&dest, lock_entry.version.as_str()) {
                    let msg = format!(
                        "{}: failed to check out {}: {e}",
                        repo_path.as_str(),
                        lock_entry.version,
                    );
                    eprintln!("rwv fetch: error: {msg}");
                    errors.push(msg);
                    continue;
                }
            }
        }

        succeeded += 1;
    }

    // Summary
    let total = succeeded + errors.len();
    if !errors.is_empty() {
        eprintln!("rwv fetch: {succeeded}/{total} repo(s) succeeded, {} failed:", errors.len());
        for msg in &errors {
            eprintln!("  - {msg}");
        }
        bail!(
            "fetch completed with {} clone failure(s) out of {total} repo(s)",
            errors.len()
        )
    }

    println!("rwv fetch: done ({succeeded} repo(s) ready)");

    // For Default mode: update rwv.lock with resolved SHAs, then auto-activate.
    if mode == FetchMode::Default {
        // Generate and write lock file (using dirty=true since we just cloned).
        let lock = lock::generate_lock(&manifest, workspace_root, None, None, true)?;
        lock::write_lock(&lock, &lock_path)?;
        eprintln!("rwv fetch: wrote {}", lock_path.display());

        // Auto-activate the project.
        let project_name = if is_url(source) {
            project_name_from_source(source)
        } else {
            // For local path, derive name from dir name.
            project_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".to_string())
        };
        crate::activate::activate(&project_name, workspace_root)?;
    }

    Ok(())
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
    fn is_url_detects_schemes() {
        assert!(is_url("https://example.com/repo"));
        assert!(is_url("file:///tmp/repo.git"));
        assert!(is_url("git@github.com:owner/repo.git"));
        assert!(!is_url("/local/path"));
        assert!(!is_url("relative/path"));
        assert!(!is_url("my-project"));
    }

    // FetchMode enum tests

    #[test]
    fn fetch_mode_variants_are_distinct() {
        assert_ne!(FetchMode::Default, FetchMode::Locked);
        assert_ne!(FetchMode::Default, FetchMode::Frozen);
        assert_ne!(FetchMode::Locked, FetchMode::Frozen);
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
        let a = FetchMode::Locked;
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
