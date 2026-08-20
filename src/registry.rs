//! Registry: maps remote hosts/paths to short local prefixes.
//!
//! A registry resolves a repo URL to a local path prefix. Built-in registries
//! handle well-known hosts; custom registries are user-configured.
//!
//! The `Registry` trait allows different hosts (GitHub, GitLab, self-hosted)
//! to have different URL parsing, authentication, and discovery behavior.

use crate::manifest::RepoUrl;
use crate::refusal::RefusalKind;

/// A short name for a code host or directory that serves as the first path
/// segment in the canonical layout: `{registry}/{owner}/{repo}/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RegistryName(String);

impl RegistryName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RegistryName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parsed identity of a repo within a registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    owner: String,
    repo: String,
}

impl RepoId {
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }
}

/// The canonical local-path shape every mint site shares:
/// `{registry}/{owner}/{repo}`.
///
/// A `String` and not a `PathBuf` because this value is an identity — the
/// manifest key `RepoPath` validates, whose separators are `/` by contract —
/// and `PathBuf::join` renders with the platform separator, which on Windows
/// mints a spelling the validator refuses.
pub(crate) fn canonical_local_path(registry: &str, owner: &str, repo: &str) -> String {
    format!("{registry}/{owner}/{repo}")
}

/// A code host or directory that can resolve URLs to local paths.
///
/// Different registries may parse URLs differently (HTTPS vs SSH vs
/// custom schemes), support different auth mechanisms, or offer API-based
/// repo discovery. The trait captures the common operations repoweave needs.
pub trait Registry {
    /// Short name used as the first path segment (e.g., `"github"`).
    fn name(&self) -> &RegistryName;

    /// If `raw` belongs to this registry, return the parsed [`RepoUrl`] variant.
    ///
    /// Each registry publishes the patterns it recognises; `RepoUrl::from_str`
    /// walks the registry list and returns the first match. Implementations
    /// return `None` for inputs they don't recognise.
    fn matches(&self, raw: &str) -> Option<RepoUrl>;

    /// Construct a clone URL from an owner/repo pair.
    ///
    /// `None` is reserved for a registry that cannot generate URLs, a
    /// directory-based one being the case in mind. No implementation returns it
    /// and every caller asserts `Some`; introducing one means revisiting those
    /// assertions, not adding a branch.
    fn clone_url(&self, id: &RepoId) -> Option<RepoUrl>;
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

    fn matches(&self, raw: &str) -> Option<RepoUrl> {
        // HTTPS: https://{domain}/owner/repo[.git]
        if let Some(path) = raw
            .strip_prefix("https://")
            .and_then(|r| r.strip_prefix(self.domain.as_str()))
            .and_then(|r| r.strip_prefix('/'))
        {
            if let Some((owner, repo)) = extract_owner_repo(path) {
                return Some(RepoUrl::Https {
                    registry: self.registry_name.clone(),
                    host: self.domain.clone(),
                    owner,
                    repo,
                });
            }
        }
        // SSH: git@{domain}:owner/repo[.git]
        if let Some(path) = raw
            .strip_prefix("git@")
            .and_then(|r| r.strip_prefix(self.domain.as_str()))
            .and_then(|r| r.strip_prefix(':'))
        {
            if let Some((owner, repo)) = extract_owner_repo(path) {
                return Some(RepoUrl::Ssh {
                    registry: self.registry_name.clone(),
                    host: self.domain.clone(),
                    owner,
                    repo,
                });
            }
        }
        // 3-part shorthand: {registry_name}/owner/repo
        let parts: Vec<&str> = raw.split('/').collect();
        if parts.len() == 3
            && parts[0] == self.registry_name.as_str()
            && !parts[1].is_empty()
            && !parts[2].is_empty()
        {
            return Some(RepoUrl::Shorthand {
                registry: Some(self.registry_name.clone()),
                owner: parts[1].to_owned(),
                repo: parts[2].to_owned(),
            });
        }
        None
    }

    fn clone_url(&self, id: &RepoId) -> Option<RepoUrl> {
        Some(RepoUrl::Https {
            registry: self.registry_name.clone(),
            host: self.domain.clone(),
            owner: id.owner().to_owned(),
            repo: id.repo().to_owned(),
        })
    }
}

/// The repo's own name at the tail of a clone source.
///
/// The last path segment with a `.git` suffix removed, reading past a
/// `git@host:owner/repo` colon as a separator the way git does. Sources that
/// no registry matched still have a nameable tail, which is what makes this
/// the fallback both `init --adopt` and `CloneInfo` extraction reach for.
pub fn repo_name_from_source(source: &str) -> String {
    let trimmed = source.trim_end_matches('/');
    let last_segment = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let last_segment = last_segment.rsplit(':').next().unwrap_or(last_segment);
    last_segment
        .strip_suffix(".git")
        .unwrap_or(last_segment)
        .to_string()
}

/// Extract `owner/repo` from the path portion of a URL. Strips a single
/// trailing `.git` suffix and ignores any segments past the first two.
fn extract_owner_repo(path: &str) -> Option<(String, String)> {
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/').filter(|s| !s.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_owned(), repo.to_owned()))
}

// ---------------------------------------------------------------------------
// Built-in registries and CloneInfo extraction
// ---------------------------------------------------------------------------

/// Result of resolving a source (URL or shorthand) to a real clone URL.
///
/// The `url` is always something `git clone` accepts — never a
/// [`RepoUrl::Shorthand`].
#[derive(Debug, Clone)]
pub struct CloneInfo {
    pub url: RepoUrl,
    pub registry: RegistryName,
    pub id: RepoId,
}

/// Extract a [`CloneInfo`] from a parsed [`RepoUrl`].
///
/// HTTPS, SSH, and File variants pass through directly — their registry, owner,
/// and repo are already known from parsing. Shorthand variants are converted
/// into a real clone URL via the named (or default) registry. Unknown variants
/// that look like URLs are accepted with a synthetic `"unknown"` registry;
/// non-URL Unknowns error.
///
/// This is the single shared resolution path used by both `fetch` and
/// `init --adopt`.
pub fn resolve_to_clone_info(source: &RepoUrl) -> anyhow::Result<CloneInfo> {
    match source {
        RepoUrl::Https {
            registry,
            owner,
            repo,
            ..
        }
        | RepoUrl::Ssh {
            registry,
            owner,
            repo,
            ..
        } => Ok(CloneInfo {
            url: source.clone(),
            registry: registry.clone(),
            id: RepoId::new(owner.as_str(), repo.as_str()),
        }),
        RepoUrl::Shorthand {
            registry,
            owner,
            repo,
        } => {
            let registries = builtin_registries();
            let target_name = match registry {
                Some(name) => name.clone(),
                None => registries
                    .first()
                    .expect("builtin_registries is never empty")
                    .name()
                    .clone(),
            };
            let reg = registries
                .iter()
                .find(|r| r.name() == &target_name)
                .expect("Shorthand registry names only ever come from a builtin registry");
            let id = RepoId::new(owner.as_str(), repo.as_str());
            let url = reg
                .clone_url(&id)
                .expect("the only Registry impl always supports clone URLs");
            Ok(CloneInfo {
                url,
                registry: target_name,
                id,
            })
        }
        RepoUrl::Unknown(s) => {
            if source.is_url() {
                let project_name = repo_name_from_source(s);
                Ok(CloneInfo {
                    url: source.clone(),
                    registry: RegistryName::new("unknown"),
                    id: RepoId::new("", project_name),
                })
            } else {
                crate::refuse!(
                    RefusalKind::UnresolvableRepoSource,
                    "cannot resolve '{}': expected a URL (https://... or git@...) or shorthand (owner/repo)",
                    s
                )
            }
        }
    }
}

/// The short names of the built-in registries — the first path segment of
/// the canonical layout, and the directory names a weave root carries.
pub fn builtin_registry_names() -> Vec<RegistryName> {
    builtin_registries()
        .iter()
        .map(|r| r.name().clone())
        .collect()
}

/// Split a canonical local path into `(registry, owner, repo)`.
///
/// The inverse of [`canonical_local_path`]. Returns `None` unless all three
/// segments are present and non-empty; anything past the third stays with
/// `repo`.
pub(crate) fn split_canonical_local_path(path: &str) -> Option<(&str, &str, &str)> {
    let mut parts = path.splitn(3, '/');
    let registry = parts.next()?;
    let owner = parts.next()?;
    let repo = parts.next()?;
    if registry.is_empty() || owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((registry, owner, repo))
}

/// Built-in registries for well-known hosts.
pub fn builtin_registries() -> Vec<Box<dyn Registry>> {
    vec![
        Box::new(DomainRegistry {
            registry_name: RegistryName::new("github"),
            domain: "github.com".into(),
        }),
        Box::new(DomainRegistry {
            registry_name: RegistryName::new("gitlab"),
            domain: "gitlab.com".into(),
        }),
        Box::new(DomainRegistry {
            registry_name: RegistryName::new("bitbucket"),
            domain: "bitbucket.org".into(),
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github_reg() -> DomainRegistry {
        DomainRegistry {
            registry_name: RegistryName::new("github"),
            domain: "github.com".into(),
        }
    }

    fn gitlab_reg() -> DomainRegistry {
        DomainRegistry {
            registry_name: RegistryName::new("gitlab"),
            domain: "gitlab.com".into(),
        }
    }

    // -----------------------------------------------------------------------
    // DomainRegistry::matches edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn domain_matches_https_trailing_slash() {
        // Trailing slash is ignored; repo = "repo"
        let url = github_reg()
            .matches("https://github.com/owner/repo/")
            .unwrap();
        assert_eq!(url.owner_repo(), Some(("owner", "repo")));
    }

    #[test]
    fn domain_matches_https_extra_path_segments() {
        // Extra segments beyond owner/repo are discarded
        let url = github_reg()
            .matches("https://github.com/owner/repo/tree/main")
            .unwrap();
        assert_eq!(url.owner_repo(), Some(("owner", "repo")));
    }

    #[test]
    fn domain_matches_ssh_extra_path_segments() {
        let url = github_reg()
            .matches("git@github.com:owner/repo/tree/main")
            .unwrap();
        assert_eq!(url.owner_repo(), Some(("owner", "repo")));
    }

    #[test]
    fn domain_matches_https_only_owner_no_repo() {
        assert!(github_reg().matches("https://github.com/owner").is_none());
    }

    #[test]
    fn domain_matches_ssh_only_owner() {
        assert!(github_reg().matches("git@github.com:owner").is_none());
    }

    #[test]
    fn domain_matches_rejects_domain_prefix_lookalike() {
        // Ensure "github.com.evil.com" doesn't match "github.com"
        assert!(github_reg()
            .matches("https://github.com.evil.com/owner/repo")
            .is_none());
    }

    #[test]
    fn domain_matches_strips_git_suffix_once() {
        let url = github_reg()
            .matches("https://github.com/owner/repo.git")
            .unwrap();
        let (_, repo) = url.owner_repo().unwrap();
        assert_eq!(repo, "repo");
    }

    #[test]
    fn domain_matches_git_in_repo_name() {
        let url = github_reg()
            .matches("https://github.com/owner/my.git.repo.git")
            .unwrap();
        let (_, repo) = url.owner_repo().unwrap();
        assert_eq!(repo, "my.git.repo");
    }

    #[test]
    fn domain_matches_three_part_shorthand_for_self() {
        let url = github_reg().matches("github/owner/repo").unwrap();
        match url {
            RepoUrl::Shorthand {
                registry: Some(r),
                owner,
                repo,
            } => {
                assert_eq!(r.as_str(), "github");
                assert_eq!(owner, "owner");
                assert_eq!(repo, "repo");
            }
            _ => panic!("expected Shorthand variant"),
        }
    }

    #[test]
    fn domain_matches_three_part_shorthand_for_other_returns_none() {
        // gitlab registry doesn't match a 3-part shorthand starting with "github"
        assert!(gitlab_reg().matches("github/owner/repo").is_none());
    }

    // -----------------------------------------------------------------------
    // repo_name_from_source
    // -----------------------------------------------------------------------

    #[test]
    fn repo_name_from_https_url() {
        assert_eq!(
            repo_name_from_source("https://github.com/org/myproject.git"),
            "myproject"
        );
    }

    #[test]
    fn repo_name_from_https_url_no_git_suffix() {
        assert_eq!(
            repo_name_from_source("https://github.com/org/myproject"),
            "myproject"
        );
    }

    #[test]
    fn repo_name_from_file_url() {
        assert_eq!(
            repo_name_from_source("file:///srv/git/project.git"),
            "project"
        );
    }

    #[test]
    fn repo_name_from_file_url_trailing_slash() {
        assert_eq!(
            repo_name_from_source("file:///srv/git/project.git/"),
            "project"
        );
    }

    #[test]
    fn repo_name_from_ssh_url() {
        assert_eq!(repo_name_from_source("git@github.com:org/repo.git"), "repo");
    }

    #[test]
    fn repo_name_from_plain_name() {
        assert_eq!(repo_name_from_source("my-project"), "my-project");
    }

    // -----------------------------------------------------------------------
    // clone_url generation
    // -----------------------------------------------------------------------

    #[test]
    fn clone_url_domain_registry() {
        let id = RepoId::new("alice", "widgets");
        assert_eq!(
            github_reg().clone_url(&id).unwrap().to_string(),
            "https://github.com/alice/widgets.git"
        );
    }

    #[test]
    fn clone_url_domain_registry_gitlab() {
        let id = RepoId::new("org", "project");
        assert_eq!(
            gitlab_reg().clone_url(&id).unwrap().to_string(),
            "https://gitlab.com/org/project.git"
        );
    }

    // -----------------------------------------------------------------------
    // RepoUrl::from_str — registry walking
    // -----------------------------------------------------------------------

    #[test]
    fn from_str_https_matches_first_registry() {
        let url: RepoUrl = "https://github.com/owner/repo.git".parse().unwrap();
        assert_eq!(url.registry(), Some(&RegistryName::new("github")));
        assert_eq!(url.owner_repo(), Some(("owner", "repo")));
        assert_eq!(url.local_path().unwrap(), "github/owner/repo");
    }

    #[test]
    fn from_str_https_matches_second_registry() {
        let url: RepoUrl = "https://gitlab.com/org/proj".parse().unwrap();
        assert_eq!(url.registry(), Some(&RegistryName::new("gitlab")));
        assert_eq!(url.owner_repo(), Some(("org", "proj")));
    }

    #[test]
    fn from_str_unmatched_url_falls_through_to_unknown() {
        let url: RepoUrl = "https://example.com/owner/repo".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
        assert!(url.is_url());
    }

    #[test]
    fn from_str_ssh_returns_correct_local_path() {
        let url: RepoUrl = "git@github.com:cwalv/repoweave.git".parse().unwrap();
        assert_eq!(url.local_path().unwrap(), "github/cwalv/repoweave");
    }

    #[test]
    fn from_str_two_part_shorthand_no_registry() {
        let url: RepoUrl = "cwalv/repoweave".parse().unwrap();
        match &url {
            RepoUrl::Shorthand {
                registry: None,
                owner,
                repo,
            } => {
                assert_eq!(owner, "cwalv");
                assert_eq!(repo, "repoweave");
            }
            _ => panic!("expected Shorthand with no registry, got {url:?}"),
        }
    }

    #[test]
    fn from_str_three_part_shorthand_named_registry() {
        let url: RepoUrl = "gitlab/org/proj".parse().unwrap();
        assert_eq!(url.registry(), Some(&RegistryName::new("gitlab")));
        assert_eq!(url.owner_repo(), Some(("org", "proj")));
        assert_eq!(url.local_path().unwrap(), "gitlab/org/proj");
    }

    #[test]
    fn from_str_three_part_bitbucket() {
        let url: RepoUrl = "bitbucket/team/repo".parse().unwrap();
        assert_eq!(url.registry(), Some(&RegistryName::new("bitbucket")));
        assert_eq!(url.owner_repo(), Some(("team", "repo")));
    }

    #[test]
    fn from_str_three_part_unknown_registry_falls_through() {
        // No registry named "unknown" is in builtin, so this becomes Unknown.
        let url: RepoUrl = "unknown/owner/repo".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
    }

    #[test]
    fn from_str_single_part_falls_through_to_unknown() {
        let url: RepoUrl = "repo".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
    }

    #[test]
    fn from_str_four_parts_falls_through_to_unknown() {
        let url: RepoUrl = "a/b/c/d".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
    }

    #[test]
    fn from_str_empty_string_falls_through_to_unknown() {
        let url: RepoUrl = "".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
    }

    #[test]
    fn from_str_empty_segments_two_part() {
        let url: RepoUrl = "/repo".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
        let url: RepoUrl = "owner/".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
    }

    #[test]
    fn from_str_empty_segments_three_part() {
        let url: RepoUrl = "github//repo".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
        let url: RepoUrl = "github/owner/".parse().unwrap();
        assert!(matches!(url, RepoUrl::Unknown(_)));
        let url: RepoUrl = "/owner/repo".parse().unwrap();
        // Parses as 3-part with empty first segment via builtin (no registry has empty name)
        // → falls through to Unknown.
        assert!(matches!(url, RepoUrl::Unknown(_)));
    }

    // -----------------------------------------------------------------------
    // RepoUrl::is_url
    // -----------------------------------------------------------------------

    #[test]
    fn is_url_excludes_shorthand() {
        let u: RepoUrl = "https://example.com/repo".parse().unwrap();
        assert!(u.is_url()); // Unknown but URL-shaped
        let u: RepoUrl = "git@github.com:owner/repo.git".parse().unwrap();
        assert!(u.is_url()); // SSH variant
        let u: RepoUrl = "owner/repo".parse().unwrap();
        assert!(!u.is_url()); // Shorthand variant
        let u: RepoUrl = "github/owner/repo".parse().unwrap();
        assert!(!u.is_url()); // Shorthand with named registry
    }

    // -----------------------------------------------------------------------
    // resolve_to_clone_info
    // -----------------------------------------------------------------------

    fn parse(s: &str) -> RepoUrl {
        s.parse().unwrap()
    }

    #[test]
    fn resolve_to_clone_info_url_known_registry() {
        let info = resolve_to_clone_info(&parse("https://github.com/org/repo.git")).unwrap();
        assert_eq!(info.url.to_string(), "https://github.com/org/repo.git");
        assert_eq!(info.registry, RegistryName::new("github"));
        assert_eq!(info.id.owner(), "org");
        assert_eq!(info.id.repo(), "repo");
    }

    #[test]
    fn resolve_to_clone_info_url_unknown_registry() {
        let info = resolve_to_clone_info(&parse("https://example.com/org/repo.git")).unwrap();
        assert_eq!(info.url.to_string(), "https://example.com/org/repo.git");
        assert_eq!(info.registry, RegistryName::new("unknown"));
        assert_eq!(info.id.repo(), "repo");
    }

    #[test]
    fn resolve_to_clone_info_ssh_url() {
        let info = resolve_to_clone_info(&parse("git@github.com:org/repo.git")).unwrap();
        assert_eq!(info.url.to_string(), "git@github.com:org/repo.git");
        assert_eq!(info.registry, RegistryName::new("github"));
        assert_eq!(info.id.owner(), "org");
        assert_eq!(info.id.repo(), "repo");
    }

    #[test]
    fn resolve_to_clone_info_two_part_shorthand() {
        let info = resolve_to_clone_info(&parse("cwalv/repoweave")).unwrap();
        assert_eq!(
            info.url.to_string(),
            "https://github.com/cwalv/repoweave.git"
        );
        assert_eq!(info.registry, RegistryName::new("github"));
        assert_eq!(info.id.owner(), "cwalv");
        assert_eq!(info.id.repo(), "repoweave");
    }

    #[test]
    fn resolve_to_clone_info_three_part_shorthand() {
        let info = resolve_to_clone_info(&parse("gitlab/org/proj")).unwrap();
        assert_eq!(info.url.to_string(), "https://gitlab.com/org/proj.git");
        assert_eq!(info.registry, RegistryName::new("gitlab"));
        assert_eq!(info.id.owner(), "org");
        assert_eq!(info.id.repo(), "proj");
    }

    #[test]
    fn resolve_to_clone_info_rejects_invalid() {
        assert!(resolve_to_clone_info(&parse("not-a-valid-source")).is_err());
    }

    #[test]
    fn resolve_to_clone_info_rejects_four_part() {
        assert!(resolve_to_clone_info(&parse("a/b/c/d")).is_err());
    }
}
