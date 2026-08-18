// ===========================================================================
// vscode-workspace — container-kind-aware regeneration of the generated
// files.exclude region
// ===========================================================================
//
// The generated exclude set is derived from a disk scan, but the file holding
// it is weave-level and committed. A container that materialized only part of
// the weave computes a strictly narrower set, so a plain replace there would
// ship the narrowing back to primary as a silent loss. Primary regeneration is
// authoritative (replace); workweave regeneration is monotone (union with the
// recorded entries this container cannot observe).

use super::*;
use repoweave::integrations::VscodeWorkspace;

/// A context whose disk view and container kind are both stated
/// explicitly. The exclude set is a function of `repos_on_disk` and
/// `project_paths`, so varying those is how these tests model a full
/// view (primary) against a partial one (workweave); `kind` is what the
/// integration actually branches on, and callers pass it independently
/// of whatever `root` looks like on disk.
#[allow(clippy::too_many_arguments)]
fn ctx_with_view<'a>(
    root: &'a Path,
    project: &'a ProjectName,
    manifest: &'a Manifest,
    config: &'a IntegrationConfig,
    cache: &'a HashMap<String, Vec<String>>,
    repos_on_disk: &'a [RepoPath],
    project_paths: &'a [String],
    kind: ContainerKind,
) -> IntegrationContext<'a> {
    IntegrationContext {
        output_dir: root,
        workspace_root: root,
        container_kind: kind,
        project,
        repos: manifest
            .iter_entries()
            .map(|(rp, e)| (rp.clone(), e.clone()))
            .collect(),
        config,
        all_repos_on_disk: repos_on_disk,
        all_project_paths: project_paths,
        detection_cache: cache,
        workweave: None,
    }
}

/// Lay down a `.rwv-workweave` marker at `root`, so the on-disk shape
/// matches a real workweave root. The container kind fed to the
/// integration under test comes from `ctx_with_view`'s `kind` argument,
/// not from this file — a resolved `Checkout` is what production code
/// threads through, and this marker is scenery for that, not the input.
fn as_workweave_root(root: &Path) {
    write_file(
        root,
        ".rwv-workweave",
        "{\"primary\":\"/elsewhere/weave\",\"project\":\"test-project\",\"parent\":\"/elsewhere/weave\"}",
    );
}

/// A `.code-workspace` as a full-view container would have written it:
/// four generated excludes, recorded in the marker, plus a user key.
const WIDE_FILE: &str = r#"{
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "rwv.generated": {
    "managed": true,
    "files.exclude": [".*", "github/other", "github/acme/legacy", "projects/sibling"]
  },
  "settings": {
    "files.exclude": {
      ".*": true,
      "github/other": true,
      "github/acme/legacy": true,
      "projects/sibling": true,
      "**/target": true
    },
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3
  }
}
"#;

fn parse(root: &Path) -> serde_json::Value {
    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    serde_json::from_str(&content).unwrap()
}

/// The keys the marker claims rwv owns, sorted.
fn marker_excludes(parsed: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = parsed["rwv.generated"]["files.exclude"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    keys.sort();
    keys
}

/// Given: a workweave that materialized one member, regenerating a file a
///        full-view container wrote.
/// Then:  every recorded entry naming a path this container does not have
///        survives — in the marker AND in the live exclude map.
#[test]
fn workweave_regen_preserves_entries_it_cannot_observe() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    as_workweave_root(root);
    write_file(root, "test-project.code-workspace", WIDE_FILE);
    std::fs::create_dir_all(root.join("github/acme/server")).unwrap();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let on_disk = vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
    let projects = vec!["test-project".to_string()];
    let ctx = ctx_with_view(
        root,
        &project,
        &manifest,
        &config,
        &cache,
        &on_disk,
        &projects,
        ContainerKind::Workweave,
    );

    VscodeWorkspace.activate(&ctx).unwrap();

    let parsed = parse(root);
    assert_eq!(
        marker_excludes(&parsed),
        vec![
            ".*",
            "github/acme/legacy",
            "github/other",
            "projects/sibling"
        ],
        "a workweave must not drop recorded entries about regions it never \
             materialized"
    );

    let exclude = &parsed["settings"]["files.exclude"];
    for key in ["github/other", "github/acme/legacy", "projects/sibling"] {
        assert_eq!(
            exclude[key],
            serde_json::Value::Bool(true),
            "preserved entry {key} must be live in the map, not only recorded"
        );
    }
    // Marker discipline is untouched: the user key still rides through.
    assert_eq!(exclude["**/target"], serde_json::Value::Bool(true));
}

/// Given: a file a full-view container authored, then regenerated by a
///        container that materialized less of the weave, nothing else
///        changed.
/// Then:  byte-identical. The diff this used to produce on every partial
///        regeneration IS the symptom — a fixpoint has nothing to stash and
///        nothing to carry back.
///
/// The prior state is authored by the code under test at a primary root
/// rather than hand-written, so the comparison is against the real
/// serialization and not a formatting accident.
#[test]
fn workweave_regen_with_no_member_change_is_a_fixpoint() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();

    // Full view: one member, one non-member repo, one sibling project.
    std::fs::create_dir_all(root.join("github/acme/server")).unwrap();
    std::fs::create_dir_all(root.join("github/other/thing")).unwrap();
    let wide_disk = vec![
        RepoPath::new("github/acme/server").expect("known-safe literal"),
        RepoPath::new("github/other/thing").expect("known-safe literal"),
    ];
    let wide_projects = vec!["test-project".to_string(), "sibling".to_string()];
    let primary_ctx = ctx_with_view(
        root,
        &project,
        &manifest,
        &config,
        &cache,
        &wide_disk,
        &wide_projects,
        ContainerKind::Primary,
    );
    VscodeWorkspace.activate(&primary_ctx).unwrap();
    let authored = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    assert_eq!(
        marker_excludes(&parse(root)),
        vec![".*", "github/other", "projects/sibling"],
        "precondition: the full view authored all three entries"
    );

    // The same file, regenerated by a container holding only the member.
    as_workweave_root(root);
    std::fs::remove_dir_all(root.join("github/other")).unwrap();
    let narrow_disk = vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
    let narrow_projects = vec!["test-project".to_string()];
    let workweave_ctx = ctx_with_view(
        root,
        &project,
        &manifest,
        &config,
        &cache,
        &narrow_disk,
        &narrow_projects,
        ContainerKind::Workweave,
    );
    VscodeWorkspace.activate(&workweave_ctx).unwrap();
    let regenerated = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();

    assert_eq!(
        authored, regenerated,
        "regeneration under a partial view must produce zero diff"
    );
}

/// Given: a workweave that HAS materialized the path an entry names, and a
///        manifest that now claims it.
/// Then:  the entry is dropped. Monotonicity defers to the recorded prior
///        only where the container has no evidence; here it has some, and
///        keeping the entry would hide a member the user can see.
#[test]
fn workweave_regen_drops_an_entry_whose_path_it_can_see() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    as_workweave_root(root);
    write_file(root, "test-project.code-workspace", WIDE_FILE);
    std::fs::create_dir_all(root.join("github/acme/server")).unwrap();
    std::fs::create_dir_all(root.join("github/acme/legacy")).unwrap();

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/legacy", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let on_disk = vec![
        RepoPath::new("github/acme/server").expect("known-safe literal"),
        RepoPath::new("github/acme/legacy").expect("known-safe literal"),
    ];
    let projects = vec!["test-project".to_string()];
    let ctx = ctx_with_view(
        root,
        &project,
        &manifest,
        &config,
        &cache,
        &on_disk,
        &projects,
        ContainerKind::Workweave,
    );

    VscodeWorkspace.activate(&ctx).unwrap();

    let parsed = parse(root);
    assert!(
        !marker_excludes(&parsed).contains(&"github/acme/legacy".to_string()),
        "an entry naming a path this container materialized is the \
             container's own business to drop: {:?}",
        marker_excludes(&parsed)
    );
    assert!(
        parsed["settings"]["files.exclude"]
            .get("github/acme/legacy")
            .is_none(),
        "the dropped entry must leave the live map too"
    );
    // The regions it still cannot observe are untouched.
    assert!(marker_excludes(&parsed).contains(&"github/other".to_string()));
}

/// Given: a primary root (no marker file) whose full scan says three of the
///        recorded entries name nothing.
/// Then:  they are dropped. Primary sees the whole weave, so an absent path
///        there is genuinely dead — the replace semantics are unchanged.
#[test]
fn primary_regen_drops_entries_for_genuinely_absent_paths() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(root, "test-project.code-workspace", WIDE_FILE);
    std::fs::create_dir_all(root.join("github/acme/server")).unwrap();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let on_disk = vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
    let projects = vec!["test-project".to_string()];
    let ctx = ctx_with_view(
        root,
        &project,
        &manifest,
        &config,
        &cache,
        &on_disk,
        &projects,
        ContainerKind::Primary,
    );

    VscodeWorkspace.activate(&ctx).unwrap();

    let parsed = parse(root);
    assert_eq!(
        marker_excludes(&parsed),
        vec![".*"],
        "primary regeneration is authoritative: entries for paths absent \
             from its full view are dead and must go"
    );
    let exclude = &parsed["settings"]["files.exclude"];
    assert!(exclude.get("github/other").is_none());
    assert!(exclude.get("github/acme/legacy").is_none());
    assert!(exclude.get("projects/sibling").is_none());
    // Still only the generated region moves.
    assert_eq!(exclude["**/target"], serde_json::Value::Bool(true));
}

/// Given: a full-view file being verified from a workweave.
/// Then:  CLEAN. The entries this container cannot observe are what a
///        regeneration here would keep, so they are not drift.
#[test]
fn verify_in_a_workweave_does_not_report_preserved_entries_as_drift() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    as_workweave_root(root);
    write_file(root, "test-project.code-workspace", WIDE_FILE);
    std::fs::create_dir_all(root.join("github/acme/server")).unwrap();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let on_disk = vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
    let projects = vec!["test-project".to_string()];
    let ctx = ctx_with_view(
        root,
        &project,
        &manifest,
        &config,
        &cache,
        &on_disk,
        &projects,
        ContainerKind::Workweave,
    );

    let issues = VscodeWorkspace.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "a workweave must not call the weave's own exclude set drift: {issues:?}"
    );
}

/// Given: a primary root whose committed file records one exclude while its
///        full scan justifies two — the shrunk state a partial regeneration
///        used to ship here.
/// Then:  DRIFT, safe to fix. Primary still reports what it can prove.
#[test]
fn verify_at_primary_reports_a_shrunk_generated_set_as_drift() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "test-project.code-workspace",
        r#"{
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "rwv.generated": { "managed": true, "files.exclude": [".*"] },
  "settings": { "files.exclude": { ".*": true } }
}
"#,
    );
    std::fs::create_dir_all(root.join("github/acme/server")).unwrap();
    std::fs::create_dir_all(root.join("github/other/thing")).unwrap();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let on_disk = vec![
        RepoPath::new("github/acme/server").expect("known-safe literal"),
        RepoPath::new("github/other/thing").expect("known-safe literal"),
    ];
    let projects = vec!["test-project".to_string()];
    let ctx = ctx_with_view(
        root,
        &project,
        &manifest,
        &config,
        &cache,
        &on_disk,
        &projects,
        ContainerKind::Primary,
    );

    let issues = VscodeWorkspace.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one DRIFT issue, got: {issues:?}"
    );
    assert!(issues[0].safe_to_fix, "DRIFT issue must be safe_to_fix");
    assert!(
        issues[0].message.contains("drift"),
        "DRIFT message should say so: {}",
        issues[0].message
    );

    // And regeneration is what settles it.
    VscodeWorkspace.activate(&ctx).unwrap();
    assert!(
        VscodeWorkspace.verify(&ctx).unwrap().is_empty(),
        "activate must clear the drift it reported"
    );
    assert_eq!(marker_excludes(&parse(root)), vec![".*", "github/other"]);
}
