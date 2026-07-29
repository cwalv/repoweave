use repoweave::manifest::RepoUrl;
use repoweave::registry::{builtin_registries, DomainRegistry, Registry, RegistryName, RepoId};
use std::path::Path;

// ---------------------------------------------------------------------------
// DomainRegistry: HTTPS URL parsing
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_parse_https_url() {
    let url: RepoUrl = "https://github.com/owner/repo.git".parse().unwrap();
    assert_eq!(url.registry(), Some(&RegistryName::new("github")));
    assert_eq!(url.owner_repo(), Some(("owner", "repo")));
}

#[test]
fn domain_registry_parse_https_url_without_git_suffix() {
    let url: RepoUrl = "https://github.com/owner/repo".parse().unwrap();
    assert_eq!(url.registry(), Some(&RegistryName::new("github")));
    assert_eq!(url.owner_repo(), Some(("owner", "repo")));
}

// ---------------------------------------------------------------------------
// DomainRegistry: SSH URL parsing
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_parse_ssh_url() {
    let url: RepoUrl = "git@github.com:owner/repo.git".parse().unwrap();
    assert!(matches!(url, RepoUrl::Ssh { .. }));
    assert_eq!(url.owner_repo(), Some(("owner", "repo")));
}

#[test]
fn domain_registry_parse_ssh_url_without_git_suffix() {
    let url: RepoUrl = "git@github.com:owner/repo".parse().unwrap();
    assert!(matches!(url, RepoUrl::Ssh { .. }));
    assert_eq!(url.owner_repo(), Some(("owner", "repo")));
}

// ---------------------------------------------------------------------------
// DomainRegistry: .git suffix handling
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_git_suffix_presence_and_absence_yield_same_result() {
    let with: RepoUrl = "https://github.com/owner/repo.git".parse().unwrap();
    let without: RepoUrl = "https://github.com/owner/repo".parse().unwrap();
    assert_eq!(with, without);
}

// ---------------------------------------------------------------------------
// DomainRegistry: reject URLs for wrong domain
// ---------------------------------------------------------------------------

#[test]
fn github_url_via_builtin_resolves_to_github_not_gitlab() {
    let url: RepoUrl = "https://gitlab.com/owner/repo.git".parse().unwrap();
    // The walked builtin list resolves gitlab URLs to the gitlab registry,
    // not github.
    assert_eq!(url.registry(), Some(&RegistryName::new("gitlab")));
}

#[test]
fn github_registry_directly_rejects_gitlab_ssh() {
    // Calling matches() directly on the github registry rejects gitlab URLs.
    let reg = github_registry();
    assert!(reg.matches("git@gitlab.com:owner/repo.git").is_none());
}

// ---------------------------------------------------------------------------
// DomainRegistry: clone_url generation
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_clone_url() {
    let reg = github_registry();
    let id = RepoId::new("cwalv", "repoweave");
    let result = reg.clone_url(&id).unwrap();
    assert_eq!(result.to_string(), "https://github.com/cwalv/repoweave.git");
}

// ---------------------------------------------------------------------------
// builtin_registries(): verify github, gitlab, bitbucket are present
// ---------------------------------------------------------------------------

#[test]
fn builtin_registries_contains_github_gitlab_bitbucket() {
    let registries = builtin_registries();
    let names: Vec<&str> = registries.iter().map(|r| r.name().as_str()).collect();
    assert!(names.contains(&"github"), "missing github");
    assert!(names.contains(&"gitlab"), "missing gitlab");
    assert!(names.contains(&"bitbucket"), "missing bitbucket");
}

#[test]
fn builtin_registries_can_parse_their_urls() {
    let github_url: RepoUrl = "https://github.com/o/r.git".parse().unwrap();
    assert_eq!(github_url.registry(), Some(&RegistryName::new("github")));
    let gitlab_url: RepoUrl = "https://gitlab.com/o/r.git".parse().unwrap();
    assert_eq!(gitlab_url.registry(), Some(&RegistryName::new("gitlab")));
    let bitbucket_url: RepoUrl = "https://bitbucket.org/o/r.git".parse().unwrap();
    assert_eq!(
        bitbucket_url.registry(),
        Some(&RegistryName::new("bitbucket"))
    );
}

// ---------------------------------------------------------------------------
// Invalid / malformed URLs at the registry level
// ---------------------------------------------------------------------------

#[test]
fn malformed_url_returns_none_from_matches() {
    let reg = github_registry();
    assert!(reg.matches("not-a-url").is_none());
    assert!(reg.matches("").is_none());
    assert!(reg.matches("ftp://github.com/owner/repo").is_none());
    assert!(reg.matches("https://").is_none());
    assert!(reg.matches("https://github.com").is_none());
    assert!(reg.matches("https://github.com/").is_none());
}

// ---------------------------------------------------------------------------
// Empty owner or repo segments return None
// ---------------------------------------------------------------------------

#[test]
fn empty_owner_returns_none() {
    let reg = github_registry();
    assert!(reg.matches("https://github.com//repo").is_none());
}

#[test]
fn empty_repo_returns_none() {
    let reg = github_registry();
    assert!(reg.matches("https://github.com/owner/").is_none());
}

#[test]
fn empty_owner_ssh_returns_none() {
    let reg = github_registry();
    assert!(reg.matches("git@github.com:/repo").is_none());
}

#[test]
fn empty_repo_ssh_returns_none() {
    let reg = github_registry();
    assert!(reg.matches("git@github.com:owner/").is_none());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn github_registry() -> DomainRegistry {
    DomainRegistry {
        registry_name: RegistryName::new("github"),
        domain: "github.com".into(),
    }
}

// ---------------------------------------------------------------------------
// 2-part shorthand: defaults to first registry at resolve time
// ---------------------------------------------------------------------------

#[test]
fn shorthand_two_part_no_registry() {
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

// ---------------------------------------------------------------------------
// 3-part shorthand: registry/owner/repo selects named registry
// ---------------------------------------------------------------------------

#[test]
fn shorthand_three_part_gitlab() {
    let url: RepoUrl = "gitlab/org/proj".parse().unwrap();
    assert_eq!(url.registry(), Some(&RegistryName::new("gitlab")));
    assert_eq!(url.owner_repo(), Some(("org", "proj")));
    assert_eq!(url.local_path().unwrap(), Path::new("gitlab/org/proj"));
}

#[test]
fn shorthand_three_part_unknown_registry_falls_through() {
    let url: RepoUrl = "sourcehut/owner/repo".parse().unwrap();
    assert!(matches!(url, RepoUrl::Unknown(_)));
}

// ---------------------------------------------------------------------------
// Invalid shorthand inputs
// ---------------------------------------------------------------------------

#[test]
fn shorthand_single_segment_falls_through_to_unknown() {
    let url: RepoUrl = "justarepo".parse().unwrap();
    assert!(matches!(url, RepoUrl::Unknown(_)));
}

#[test]
fn shorthand_too_many_segments_falls_through_to_unknown() {
    let url: RepoUrl = "a/b/c/d".parse().unwrap();
    assert!(matches!(url, RepoUrl::Unknown(_)));
}
