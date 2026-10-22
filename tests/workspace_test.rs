use repoweave::manifest::{ProjectName, WorkweaveId, WorkweaveName};
use repoweave::workspace::{observe_root, workweave_name_in, PrimaryIdentity, WeaveRootIdentity};

mod common;

// ============================================================================
// WorkweaveId::flat — the "{project}--{workweave}" directory name
// ============================================================================

fn weave_dir_name(project: &ProjectName, workweave: &WorkweaveName) -> String {
    WorkweaveId {
        project: project.clone(),
        name: workweave.clone(),
    }
    .flat()
    .to_string()
}

#[test]
fn weave_dir_name_simple() {
    let name = weave_dir_name(
        &ProjectName::new("web-app").unwrap(),
        &WorkweaveName::new("agent-42").unwrap(),
    );
    assert_eq!(name, "web-app--agent-42");
}

#[test]
fn weave_dir_name_single_word_components() {
    let name = weave_dir_name(
        &ProjectName::new("myproject").unwrap(),
        &WorkweaveName::new("hotfix").unwrap(),
    );
    assert_eq!(name, "myproject--hotfix");
}

#[test]
fn weave_dir_name_complex_primary() {
    let name = weave_dir_name(
        &ProjectName::new("my-complex-project").unwrap(),
        &WorkweaveName::new("feat-login").unwrap(),
    );
    assert_eq!(name, "my-complex-project--feat-login");
}

#[test]
fn weave_dir_name_weave_with_numbers() {
    let name = weave_dir_name(
        &ProjectName::new("app").unwrap(),
        &WorkweaveName::new("issue-1234").unwrap(),
    );
    assert_eq!(name, "app--issue-1234");
}

// ============================================================================
// workweave_name_in — the name half, read against the project that rendered it
// ============================================================================

#[test]
fn name_half_of_a_directory_this_project_rendered() {
    let name = workweave_name_in(&ProjectName::new("web-app").unwrap(), "web-app--agent-42");
    assert_eq!(name, Some(WorkweaveName::new("agent-42").unwrap()));
}

#[test]
fn name_half_when_both_halves_are_single_words() {
    let name = workweave_name_in(&ProjectName::new("proj").unwrap(), "proj--fix");
    assert_eq!(name, Some(WorkweaveName::new("fix").unwrap()));
}

#[test]
fn name_half_when_both_halves_are_hyphenated() {
    let name = workweave_name_in(&ProjectName::new("my-app").unwrap(), "my-app--my-feature");
    assert_eq!(name, Some(WorkweaveName::new("my-feature").unwrap()));
}

/// The project is the split point, so a directory another project rendered has
/// no name half here however well-formed it looks. This is what a marker-held
/// project buys over a guess at the first separator: `doctor --fix` writes the
/// answer into the registry, and a directory belonging to someone else must
/// produce nothing rather than a plausible name.
#[test]
fn a_directory_another_project_rendered_has_no_name_half() {
    let proj = ProjectName::new("proj").unwrap();
    assert!(workweave_name_in(&proj, "other--fix").is_none());
    assert!(workweave_name_in(&proj, "proj-extra--fix").is_none());
    assert!(workweave_name_in(&ProjectName::new("a/b").unwrap(), "a--b--fix").is_none());
}

// ============================================================================
// workweave_name_in — edge cases
// ============================================================================

#[test]
fn no_separator_has_no_name_half() {
    let proj = ProjectName::new("web-app").unwrap();
    assert!(workweave_name_in(&proj, "web-app").is_none());
    assert!(workweave_name_in(&proj, "web-app-feature").is_none());
    assert!(workweave_name_in(&proj, "").is_none());
}

#[test]
fn an_empty_name_half_is_not_a_name() {
    assert!(workweave_name_in(&ProjectName::new("primary").unwrap(), "primary--").is_none());
}

/// `a--b--c` read against project `a` offers `b--c`, which a workweave name
/// may not be. Reading it without a project is what would let it be taken for
/// project `a`, workweave `b--c` — or for project `a--b`, workweave `c` — and
/// the two are indistinguishable from the string.
#[test]
fn a_name_half_that_spells_the_separator_is_rejected() {
    assert!(workweave_name_in(&ProjectName::new("a").unwrap(), "a--b--c").is_none());
    assert!(workweave_name_in(&ProjectName::new("proj").unwrap(), "proj--feat--v2--rc1").is_none());
}

// ============================================================================
// Round-trip: WorkweaveId::flat -> workweave_name_in
// ============================================================================

#[test]
fn round_trip_simple() {
    let primary = ProjectName::new("web-app").unwrap();
    let workweave = WorkweaveName::new("agent-42").unwrap();
    let dir_name = weave_dir_name(&primary, &workweave);
    assert_eq!(workweave_name_in(&primary, &dir_name), Some(workweave));
}

#[test]
fn round_trip_single_char_components() {
    let primary = ProjectName::new("a").unwrap();
    let workweave = WorkweaveName::new("b").unwrap();
    let dir_name = weave_dir_name(&primary, &workweave);
    assert_eq!(workweave_name_in(&primary, &dir_name), Some(workweave));
}

// ============================================================================
// PrimaryIdentity::select_project — writes .rwv-active file
// ============================================================================

/// A workspace-shaped root, which is what `observe_root` needs before it will
/// classify a directory as primary at all. The bare tempdir these tests used
/// while selection took a `&Path` no longer reaches the write.
fn primary_root(parent: &std::path::Path) -> std::path::PathBuf {
    let root = parent.join("ws");
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

fn witness(root: &std::path::Path) -> PrimaryIdentity {
    match observe_root(root).unwrap().require_exclusive() {
        Ok(WeaveRootIdentity::Primary(identity)) => identity,
        other => panic!("{} is not a primary root: {other:?}", root.display()),
    }
}

#[test]
fn select_project_creates_file() {
    let tmp = common::tempdir().unwrap();
    let root = primary_root(tmp.path());
    let project = ProjectName::new("web-app").unwrap();
    witness(&root).select_project(&project).unwrap();
    let content = std::fs::read_to_string(root.join(".rwv-active")).unwrap();
    assert_eq!(content, "web-app\n");
}

#[test]
fn select_project_round_trips_with_read() {
    let tmp = common::tempdir().unwrap();
    let root = primary_root(tmp.path());
    let project = ProjectName::new("mobile-app").unwrap();
    witness(&root).select_project(&project).unwrap();
    assert_eq!(witness(&root).pointer().named_project(), Some(&project));
}
