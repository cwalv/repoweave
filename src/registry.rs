//! Registry: maps remote hosts/paths to short local prefixes.
//!
//! A registry resolves a repo URL to a local path prefix. Built-in registries
//! handle well-known hosts; custom registries are user-configured.
//!
//! The `Registry` trait allows different hosts (GitHub, GitLab, self-hosted)
//! to have different URL parsing, authentication, and discovery behavior.

use std::path::{Path, PathBuf};

/// A short name for a code host or directory that serves as the first path
/// segment in the canonical layout: `{registry}/{owner}/{repo}/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RegistryName(pub String);

/// Parsed identity of a repo within a registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    pub owner: String,
    pub repo: String,
}

/// A code host or directory that can resolve URLs to local paths.
///
/// Different registries may parse URLs differently (HTTPS vs SSH vs
/// custom schemes), support different auth mechanisms, or offer API-based
/// repo discovery. The trait captures the common operations repoweave needs.
pub trait Registry {
    /// Short name used as the first path segment (e.g., `"github"`).
    fn name(&self) -> &RegistryName;

    /// If `url` belongs to this registry, parse out the owner and repo.
    fn parse_url(&self, url: &str) -> Option<RepoId>;

    /// Construct a clone URL from an owner/repo pair.
    /// Returns `None` if this registry can't generate URLs (e.g., directory-based).
    fn clone_url(&self, id: &RepoId) -> Option<String>;

    /// The local path for a repo: `{registry}/{owner}/{repo}`.
    fn local_path(&self, id: &RepoId) -> PathBuf {
        Path::new(&self.name().0).join(&id.owner).join(&id.repo)
    }
}

// ---------------------------------------------------------------------------
// Domain-based registry (GitHub, GitLab, Bitbucket, self-hosted)
// ---------------------------------------------------------------------------

/// A registry that matches URLs by domain name.
/// Handles `https://{domain}/owner/repo.git` and `git@{domain}:owner/repo.git`.
pub struct DomainRegistry {
    pub registry_name: RegistryName,
    pub domain: String,
}

impl Registry for DomainRegistry {
    fn name(&self) -> &RegistryName {
        &self.registry_name
    }

    fn parse_url(&self, url: &str) -> Option<RepoId> {
        // HTTPS: https://github.com/owner/repo.git
        // SSH:   git@github.com:owner/repo.git
        let path = if let Some(rest) = url.strip_prefix("https://") {
            let rest = rest.strip_prefix(self.domain.as_str())?;
            rest.strip_prefix('/')
        } else if let Some(rest) = url.strip_prefix("git@") {
            let rest = rest.strip_prefix(self.domain.as_str())?;
            rest.strip_prefix(':')
        } else {
            None
        }?;

        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.split('/').filter(|s| !s.is_empty());
        let owner = parts.next()?.to_string();
        let repo = parts.next()?.to_string();
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        Some(RepoId { owner, repo })
    }

    fn clone_url(&self, id: &RepoId) -> Option<String> {
        Some(format!(
            "https://{}/{}/{}.git",
            self.domain, id.owner, id.repo
        ))
    }
}

// ---------------------------------------------------------------------------
// Directory-based registry (local repos under a shared prefix)
// ---------------------------------------------------------------------------

/// A registry that matches `file://` URLs under a local directory prefix.
pub struct DirectoryRegistry {
    pub registry_name: RegistryName,
    pub prefix: PathBuf,
}

impl Registry for DirectoryRegistry {
    fn name(&self) -> &RegistryName {
        &self.registry_name
    }

    fn parse_url(&self, url: &str) -> Option<RepoId> {
        let path = url.strip_prefix("file://")?;
        let path = Path::new(path);
        let relative = path.strip_prefix(&self.prefix).ok()?;
        let mut components = relative.components();
        let owner = components.next()?.as_os_str().to_str()?.to_string();
        let repo = components.next()?.as_os_str().to_str()?.to_string();
        Some(RepoId { owner, repo })
    }

    fn clone_url(&self, _id: &RepoId) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// Built-in registries
// ---------------------------------------------------------------------------

/// Try each registry in order and return the first that can parse `url`.
///
/// Returns the registry name, parsed repo ID, and the local path for the repo.
pub fn resolve_url(
    url: &str,
    registries: &[&dyn Registry],
) -> Option<(RegistryName, RepoId, PathBuf)> {
    for reg in registries {
        if let Some(id) = reg.parse_url(url) {
            let path = reg.local_path(&id);
            return Some((reg.name().clone(), id, path));
        }
    }
    None
}

/// Resolve a shorthand string to a registry, repo ID, and local path.
///
/// Accepts two forms:
/// - **2-part** (`owner/repo`): resolves via the default (first) registry.
/// - **3-part** (`registry/owner/repo`): resolves via the named registry.
///
/// Returns `None` if the shorthand is malformed, the named registry is not
/// found, or the registry list is empty (for 2-part).
pub fn resolve_shorthand(
    shorthand: &str,
    registries: &[&dyn Registry],
) -> Option<(RegistryName, RepoId, PathBuf)> {
    let parts: Vec<&str> = shorthand.split('/').collect();
    match parts.len() {
        2 => {
            let owner = parts[0];
            let repo = parts[1];
            if owner.is_empty() || repo.is_empty() {
                return None;
            }
            // Use the default (first) registry
            let reg = registries.first()?;
            let id = RepoId {
                owner: owner.to_string(),
                repo: repo.to_string(),
            };
            let path = reg.local_path(&id);
            Some((reg.name().clone(), id, path))
        }
        3 => {
            let registry_name = parts[0];
            let owner = parts[1];
            let repo = parts[2];
            if registry_name.is_empty() || owner.is_empty() || repo.is_empty() {
                return None;
            }
            let reg = registries.iter().find(|r| r.name().0 == registry_name)?;
            let id = RepoId {
                owner: owner.to_string(),
                repo: repo.to_string(),
            };
            let path = reg.local_path(&id);
            Some((reg.name().clone(), id, path))
        }
        _ => None,
    }
}

/// Returns true if `source` looks like a URL (as opposed to a shorthand).
fn is_url(source: &str) -> bool {
    source.contains("://") || source.starts_with("git@")
}

/// Resolve a source string (URL or shorthand) to its clone URL, registry name,
/// and repo ID.
///
/// For full URLs, the clone URL is the source itself. The registry name and repo
/// ID are populated when a registry can parse the URL, or defaulted to a
/// synthetic `RegistryName("unknown")` with the repo name derived from the last
/// path segment.
///
/// For shorthands (`owner/repo` or `registry/owner/repo`), the registry
/// constructs the clone URL.
///
/// This is the single shared resolution path used by both `fetch` and `init --adopt`.
pub fn resolve_to_clone_info(source: &str) -> anyhow::Result<(String, RegistryName, RepoId)> {
    let registries = builtin_registries();
    let refs: Vec<&dyn Registry> = registries.iter().map(|r| r.as_ref()).collect();

    if is_url(source) {
        // For URLs, try to parse via registries to get structured info.
        if let Some((name, id, _path)) = resolve_url(source, &refs) {
            return Ok((source.to_string(), name, id));
        }
        // No registry matched — derive repo name from the URL.
        let project_name = crate::fetch::project_name_from_source(source);
        return Ok((
            source.to_string(),
            RegistryName("unknown".into()),
            RepoId {
                owner: String::new(),
                repo: project_name,
            },
        ));
    }

    // Try as a shorthand (owner/repo or registry/owner/repo).
    if let Some((name, id, _path)) = resolve_shorthand(source, &refs) {
        let reg = refs
            .iter()
            .find(|r| r.name() == &name)
            .ok_or_else(|| anyhow::anyhow!("registry '{}' not found", name.0))?;
        let url = reg
            .clone_url(&id)
            .ok_or_else(|| anyhow::anyhow!("registry '{}' does not support clone URLs", name.0))?;
        return Ok((url, name, id));
    }

    anyhow::bail!(
        "cannot resolve '{}': expected a URL (https://... or git@...) or shorthand (owner/repo)",
        source
    )
}

/// Built-in registries for well-known hosts.
pub fn builtin_registries() -> Vec<Box<dyn Registry>> {
    vec![
        Box::new(DomainRegistry {
            registry_name: RegistryName("github".into()),
            domain: "github.com".into(),
        }),
        Box::new(DomainRegistry {
            registry_name: RegistryName("gitlab".into()),
            domain: "gitlab.com".into(),
        }),
        Box::new(DomainRegistry {
            registry_name: RegistryName("bitbucket".into()),
            domain: "bitbucket.org".into(),
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github_reg() -> DomainRegistry {
        DomainRegistry {
            registry_name: RegistryName("github".into()),
            domain: "github.com".into(),
        }
    }

    fn gitlab_reg() -> DomainRegistry {
        DomainRegistry {
            registry_name: RegistryName("gitlab".into()),
            domain: "gitlab.com".into(),
        }
    }

    fn dir_reg() -> DirectoryRegistry {
        DirectoryRegistry {
            registry_name: RegistryName("local".into()),
            prefix: PathBuf::from("/srv/repos"),
        }
    }

    // -----------------------------------------------------------------------
    // parse_url edge cases for DomainRegistry
    // -----------------------------------------------------------------------

    #[test]
    fn parse_domain_url_trailing_slash_https() {
        let reg = github_reg();
        // Trailing slash is ignored; repo = "repo"
        let id = reg.parse_url("https://github.com/owner/repo/").unwrap();
        assert_eq!(id.owner, "owner");
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn parse_domain_url_extra_path_segments_https() {
        let reg = github_reg();
        // Extra segments beyond owner/repo are discarded
        let id = reg
            .parse_url("https://github.com/owner/repo/tree/main")
            .unwrap();
        assert_eq!(id.owner, "owner");
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn parse_domain_url_extra_path_segments_ssh() {
        let reg = github_reg();
        // Extra segments beyond owner/repo are discarded
        let id = reg
            .parse_url("git@github.com:owner/repo/tree/main")
            .unwrap();
        assert_eq!(id.owner, "owner");
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn parse_domain_url_only_owner_no_repo() {
        let reg = github_reg();
        assert!(reg.parse_url("https://github.com/owner").is_none());
    }

    #[test]
    fn parse_domain_url_ssh_only_owner() {
        let reg = github_reg();
        assert!(reg.parse_url("git@github.com:owner").is_none());
    }

    #[test]
    fn parse_domain_url_domain_prefix_match_rejected() {
        // Ensure "github.com.evil.com" doesn't match "github.com"
        let reg = github_reg();
        assert!(reg
            .parse_url("https://github.com.evil.com/owner/repo")
            .is_none());
    }

    #[test]
    fn parse_domain_url_strips_git_suffix_once() {
        let reg = github_reg();
        let id = reg.parse_url("https://github.com/owner/repo.git").unwrap();
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn parse_domain_url_git_in_repo_name() {
        let reg = github_reg();
        let id = reg
            .parse_url("https://github.com/owner/my.git.repo.git")
            .unwrap();
        assert_eq!(id.repo, "my.git.repo");
    }

    // -----------------------------------------------------------------------
    // parse_url edge cases for DirectoryRegistry
    // -----------------------------------------------------------------------

    #[test]
    fn parse_directory_url_extra_segments() {
        let reg = dir_reg();
        // Extra path segments beyond owner/repo are ignored (components iterator)
        let id = reg
            .parse_url("file:///srv/repos/owner/repo/sub/dir")
            .unwrap();
        assert_eq!(id.owner, "owner");
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn parse_directory_url_only_owner() {
        let reg = dir_reg();
        assert!(reg.parse_url("file:///srv/repos/owner").is_none());
    }

    #[test]
    fn parse_directory_url_exact_prefix_no_segments() {
        let reg = dir_reg();
        assert!(reg.parse_url("file:///srv/repos").is_none());
    }

    #[test]
    fn parse_directory_url_trailing_slash() {
        let reg = dir_reg();
        // "/srv/repos/owner/repo/" — the trailing slash doesn't add a component
        let id = reg.parse_url("file:///srv/repos/owner/repo/").unwrap();
        assert_eq!(id.owner, "owner");
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn parse_directory_url_non_file_scheme() {
        let reg = dir_reg();
        assert!(reg.parse_url("https:///srv/repos/owner/repo").is_none());
    }

    // -----------------------------------------------------------------------
    // local_path generation
    // -----------------------------------------------------------------------

    #[test]
    fn local_path_domain_registry() {
        let reg = github_reg();
        let id = RepoId {
            owner: "alice".into(),
            repo: "widgets".into(),
        };
        assert_eq!(reg.local_path(&id), Path::new("github/alice/widgets"));
    }

    #[test]
    fn local_path_directory_registry() {
        let reg = dir_reg();
        let id = RepoId {
            owner: "bob".into(),
            repo: "tools".into(),
        };
        assert_eq!(reg.local_path(&id), Path::new("local/bob/tools"));
    }

    // -----------------------------------------------------------------------
    // clone_url generation
    // -----------------------------------------------------------------------

    #[test]
    fn clone_url_domain_registry() {
        let reg = github_reg();
        let id = RepoId {
            owner: "alice".into(),
            repo: "widgets".into(),
        };
        assert_eq!(
            reg.clone_url(&id).unwrap(),
            "https://github.com/alice/widgets.git"
        );
    }

    #[test]
    fn clone_url_domain_registry_gitlab() {
        let reg = gitlab_reg();
        let id = RepoId {
            owner: "org".into(),
            repo: "project".into(),
        };
        assert_eq!(
            reg.clone_url(&id).unwrap(),
            "https://gitlab.com/org/project.git"
        );
    }

    #[test]
    fn clone_url_directory_registry_returns_none() {
        let reg = dir_reg();
        let id = RepoId {
            owner: "x".into(),
            repo: "y".into(),
        };
        assert!(reg.clone_url(&id).is_none());
    }

    // -----------------------------------------------------------------------
    // resolve_url
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_url_first_match_wins() {
        let gh = github_reg();
        let gl = gitlab_reg();
        let registries: Vec<&dyn Registry> = vec![&gh, &gl];

        let (name, id, path) =
            resolve_url("https://github.com/owner/repo.git", &registries).unwrap();
        assert_eq!(name, RegistryName("github".into()));
        assert_eq!(id.owner, "owner");
        assert_eq!(id.repo, "repo");
        assert_eq!(path, Path::new("github/owner/repo"));
    }

    #[test]
    fn resolve_url_second_registry_matches() {
        let gh = github_reg();
        let gl = gitlab_reg();
        let registries: Vec<&dyn Registry> = vec![&gh, &gl];

        let (name, id, _path) = resolve_url("https://gitlab.com/org/proj", &registries).unwrap();
        assert_eq!(name, RegistryName("gitlab".into()));
        assert_eq!(id.owner, "org");
        assert_eq!(id.repo, "proj");
    }

    #[test]
    fn resolve_url_no_match_returns_none() {
        let gh = github_reg();
        let gl = gitlab_reg();
        let registries: Vec<&dyn Registry> = vec![&gh, &gl];

        assert!(resolve_url("https://example.com/owner/repo", &registries).is_none());
    }

    #[test]
    fn resolve_url_empty_registries_returns_none() {
        let registries: Vec<&dyn Registry> = vec![];
        assert!(resolve_url("https://github.com/owner/repo", &registries).is_none());
    }

    #[test]
    fn resolve_url_with_directory_registry() {
        let dr = dir_reg();
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&dr, &gh];

        let (name, id, path) = resolve_url("file:///srv/repos/owner/repo", &registries).unwrap();
        assert_eq!(name, RegistryName("local".into()));
        assert_eq!(id.owner, "owner");
        assert_eq!(id.repo, "repo");
        assert_eq!(path, Path::new("local/owner/repo"));
    }

    #[test]
    fn resolve_url_returns_correct_local_path() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];

        let (_name, _id, path) =
            resolve_url("git@github.com:cwalv/repoweave.git", &registries).unwrap();
        assert_eq!(path, Path::new("github/cwalv/repoweave"));
    }

    // -----------------------------------------------------------------------
    // resolve_shorthand
    // -----------------------------------------------------------------------

    /// Convert `Vec<Box<dyn Registry>>` to `Vec<&dyn Registry>` for tests.
    fn as_refs(registries: &[Box<dyn Registry>]) -> Vec<&dyn Registry> {
        registries.iter().map(|r| r.as_ref()).collect()
    }

    #[test]
    fn resolve_shorthand_two_part_uses_default_registry() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        let (name, id, path) = resolve_shorthand("cwalv/repoweave", &registries).unwrap();
        assert_eq!(name, RegistryName("github".into()));
        assert_eq!(id.owner, "cwalv");
        assert_eq!(id.repo, "repoweave");
        assert_eq!(path, Path::new("github/cwalv/repoweave"));
    }

    #[test]
    fn resolve_shorthand_three_part_selects_named_registry() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        let (name, id, path) = resolve_shorthand("gitlab/org/proj", &registries).unwrap();
        assert_eq!(name, RegistryName("gitlab".into()));
        assert_eq!(id.owner, "org");
        assert_eq!(id.repo, "proj");
        assert_eq!(path, Path::new("gitlab/org/proj"));
    }

    #[test]
    fn resolve_shorthand_three_part_bitbucket() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        let (name, id, _path) = resolve_shorthand("bitbucket/team/repo", &registries).unwrap();
        assert_eq!(name, RegistryName("bitbucket".into()));
        assert_eq!(id.owner, "team");
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn resolve_shorthand_three_part_unknown_registry_returns_none() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        assert!(resolve_shorthand("unknown/owner/repo", &registries).is_none());
    }

    #[test]
    fn resolve_shorthand_single_part_returns_none() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        assert!(resolve_shorthand("repo", &registries).is_none());
    }

    #[test]
    fn resolve_shorthand_four_parts_returns_none() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        assert!(resolve_shorthand("a/b/c/d", &registries).is_none());
    }

    #[test]
    fn resolve_shorthand_empty_string_returns_none() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        assert!(resolve_shorthand("", &registries).is_none());
    }

    #[test]
    fn resolve_shorthand_empty_segments_two_part() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        assert!(resolve_shorthand("/repo", &registries).is_none());
        assert!(resolve_shorthand("owner/", &registries).is_none());
    }

    #[test]
    fn resolve_shorthand_empty_segments_three_part() {
        let owned = builtin_registries();
        let registries = as_refs(&owned);
        assert!(resolve_shorthand("github//repo", &registries).is_none());
        assert!(resolve_shorthand("github/owner/", &registries).is_none());
        assert!(resolve_shorthand("/owner/repo", &registries).is_none());
    }

    #[test]
    fn resolve_shorthand_empty_registries_returns_none() {
        let registries: Vec<&dyn Registry> = vec![];
        assert!(resolve_shorthand("owner/repo", &registries).is_none());
    }

    // -----------------------------------------------------------------------
    // is_url
    // -----------------------------------------------------------------------

    #[test]
    fn is_url_detects_schemes() {
        assert!(is_url("https://example.com/repo"));
        assert!(is_url("file:///tmp/repo.git"));
        assert!(is_url("git@github.com:owner/repo.git"));
        assert!(!is_url("owner/repo"));
        assert!(!is_url("github/owner/repo"));
        assert!(!is_url("my-project"));
    }

    // -----------------------------------------------------------------------
    // resolve_to_clone_info
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_to_clone_info_url_known_registry() {
        let (url, name, id) = resolve_to_clone_info("https://github.com/org/repo.git").unwrap();
        assert_eq!(url, "https://github.com/org/repo.git");
        assert_eq!(name, RegistryName("github".into()));
        assert_eq!(id.owner, "org");
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn resolve_to_clone_info_url_unknown_registry() {
        let (url, name, id) = resolve_to_clone_info("https://example.com/org/repo.git").unwrap();
        assert_eq!(url, "https://example.com/org/repo.git");
        assert_eq!(name, RegistryName("unknown".into()));
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn resolve_to_clone_info_ssh_url() {
        let (url, name, id) = resolve_to_clone_info("git@github.com:org/repo.git").unwrap();
        assert_eq!(url, "git@github.com:org/repo.git");
        assert_eq!(name, RegistryName("github".into()));
        assert_eq!(id.owner, "org");
        assert_eq!(id.repo, "repo");
    }

    #[test]
    fn resolve_to_clone_info_two_part_shorthand() {
        let (url, name, id) = resolve_to_clone_info("cwalv/repoweave").unwrap();
        assert_eq!(url, "https://github.com/cwalv/repoweave.git");
        assert_eq!(name, RegistryName("github".into()));
        assert_eq!(id.owner, "cwalv");
        assert_eq!(id.repo, "repoweave");
    }

    #[test]
    fn resolve_to_clone_info_three_part_shorthand() {
        let (url, name, id) = resolve_to_clone_info("gitlab/org/proj").unwrap();
        assert_eq!(url, "https://gitlab.com/org/proj.git");
        assert_eq!(name, RegistryName("gitlab".into()));
        assert_eq!(id.owner, "org");
        assert_eq!(id.repo, "proj");
    }

    #[test]
    fn resolve_to_clone_info_rejects_invalid() {
        assert!(resolve_to_clone_info("not-a-valid-source").is_err());
    }

    #[test]
    fn resolve_to_clone_info_rejects_four_part() {
        assert!(resolve_to_clone_info("a/b/c/d").is_err());
    }
}
