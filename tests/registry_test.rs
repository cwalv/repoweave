use repoweave::registry::{
    builtin_registries, DirectoryRegistry, DomainRegistry, Registry, RegistryName, RepoId,
};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// DomainRegistry: HTTPS URL parsing
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_parse_https_url() {
    let reg = github_registry();
    let id = reg.parse_url("https://github.com/owner/repo.git").unwrap();
    assert_eq!(id.owner, "owner");
    assert_eq!(id.repo, "repo");
}

#[test]
fn domain_registry_parse_https_url_without_git_suffix() {
    let reg = github_registry();
    let id = reg.parse_url("https://github.com/owner/repo").unwrap();
    assert_eq!(id.owner, "owner");
    assert_eq!(id.repo, "repo");
}

// ---------------------------------------------------------------------------
// DomainRegistry: SSH URL parsing
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_parse_ssh_url() {
    let reg = github_registry();
    let id = reg.parse_url("git@github.com:owner/repo.git").unwrap();
    assert_eq!(id.owner, "owner");
    assert_eq!(id.repo, "repo");
}

#[test]
fn domain_registry_parse_ssh_url_without_git_suffix() {
    let reg = github_registry();
    let id = reg.parse_url("git@github.com:owner/repo").unwrap();
    assert_eq!(id.owner, "owner");
    assert_eq!(id.repo, "repo");
}

// ---------------------------------------------------------------------------
// DomainRegistry: .git suffix handling
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_git_suffix_presence_and_absence_yield_same_result() {
    let reg = github_registry();
    let with = reg.parse_url("https://github.com/owner/repo.git").unwrap();
    let without = reg.parse_url("https://github.com/owner/repo").unwrap();
    assert_eq!(with, without);
}

// ---------------------------------------------------------------------------
// DomainRegistry: reject URLs for wrong domain
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_rejects_wrong_domain_https() {
    let reg = github_registry();
    assert!(reg.parse_url("https://gitlab.com/owner/repo.git").is_none());
}

#[test]
fn domain_registry_rejects_wrong_domain_ssh() {
    let reg = github_registry();
    assert!(reg.parse_url("git@gitlab.com:owner/repo.git").is_none());
}

// ---------------------------------------------------------------------------
// DomainRegistry: clone_url generation
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_clone_url() {
    let reg = github_registry();
    let id = RepoId {
        owner: "cwalv".into(),
        repo: "repoweave".into(),
    };
    let url = reg.clone_url(&id).unwrap();
    assert_eq!(url, "https://github.com/cwalv/repoweave.git");
}

// ---------------------------------------------------------------------------
// DomainRegistry: local_path generation
// ---------------------------------------------------------------------------

#[test]
fn domain_registry_local_path() {
    let reg = github_registry();
    let id = RepoId {
        owner: "cwalv".into(),
        repo: "repoweave".into(),
    };
    let path = reg.local_path(&id);
    assert_eq!(path, Path::new("github/cwalv/repoweave"));
}

// ---------------------------------------------------------------------------
// DirectoryRegistry: parse file:// URLs
// ---------------------------------------------------------------------------

#[test]
fn directory_registry_parse_file_url() {
    let reg = dir_registry();
    let id = reg
        .parse_url("file:///srv/repos/owner/repo")
        .unwrap();
    assert_eq!(id.owner, "owner");
    assert_eq!(id.repo, "repo");
}

// ---------------------------------------------------------------------------
// DirectoryRegistry: reject non-matching prefixes
// ---------------------------------------------------------------------------

#[test]
fn directory_registry_rejects_non_matching_prefix() {
    let reg = dir_registry();
    assert!(reg.parse_url("file:///other/path/owner/repo").is_none());
}

#[test]
fn directory_registry_rejects_https_urls() {
    let reg = dir_registry();
    assert!(reg.parse_url("https://example.com/owner/repo").is_none());
}

// ---------------------------------------------------------------------------
// DirectoryRegistry: clone_url returns None
// ---------------------------------------------------------------------------

#[test]
fn directory_registry_clone_url_returns_none() {
    let reg = dir_registry();
    let id = RepoId {
        owner: "owner".into(),
        repo: "repo".into(),
    };
    assert!(reg.clone_url(&id).is_none());
}

// ---------------------------------------------------------------------------
// builtin_registries(): verify github, gitlab, bitbucket are present
// ---------------------------------------------------------------------------

#[test]
fn builtin_registries_contains_github_gitlab_bitbucket() {
    let registries = builtin_registries();
    let names: Vec<&str> = registries.iter().map(|r| r.name().0.as_str()).collect();
    assert!(names.contains(&"github"), "missing github");
    assert!(names.contains(&"gitlab"), "missing gitlab");
    assert!(names.contains(&"bitbucket"), "missing bitbucket");
}

#[test]
fn builtin_registries_can_parse_their_urls() {
    let registries = builtin_registries();
    // github
    assert!(registries[0]
        .parse_url("https://github.com/o/r.git")
        .is_some());
    // gitlab
    assert!(registries[1]
        .parse_url("https://gitlab.com/o/r.git")
        .is_some());
    // bitbucket
    assert!(registries[2]
        .parse_url("https://bitbucket.org/o/r.git")
        .is_some());
}

// ---------------------------------------------------------------------------
// Invalid / malformed URLs return None
// ---------------------------------------------------------------------------

#[test]
fn malformed_url_returns_none() {
    let reg = github_registry();
    assert!(reg.parse_url("not-a-url").is_none());
    assert!(reg.parse_url("").is_none());
    assert!(reg.parse_url("ftp://github.com/owner/repo").is_none());
    assert!(reg.parse_url("https://").is_none());
    assert!(reg.parse_url("https://github.com").is_none());
    assert!(reg.parse_url("https://github.com/").is_none());
}

// ---------------------------------------------------------------------------
// Empty owner or repo segments return None
// ---------------------------------------------------------------------------

#[test]
fn empty_owner_returns_none() {
    let reg = github_registry();
    assert!(reg.parse_url("https://github.com//repo").is_none());
}

#[test]
fn empty_repo_returns_none() {
    let reg = github_registry();
    assert!(reg.parse_url("https://github.com/owner/").is_none());
}

#[test]
fn empty_owner_ssh_returns_none() {
    let reg = github_registry();
    assert!(reg.parse_url("git@github.com:/repo").is_none());
}

#[test]
fn empty_repo_ssh_returns_none() {
    let reg = github_registry();
    assert!(reg.parse_url("git@github.com:owner/").is_none());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn github_registry() -> DomainRegistry {
    DomainRegistry {
        registry_name: RegistryName("github".into()),
        domain: "github.com".into(),
    }
}

fn dir_registry() -> DirectoryRegistry {
    DirectoryRegistry {
        registry_name: RegistryName("local".into()),
        prefix: PathBuf::from("/srv/repos"),
    }
}
