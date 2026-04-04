use repoweave::manifest::{ProjectName, WeaveName};
use repoweave::workspace::{
    parse_weave_dir_name, read_active_project, set_active_project, weave_dir_name,
};

// ============================================================================
// weave_dir_name — generates "{primary}--{weave}" directory names
// ============================================================================

#[test]
fn weave_dir_name_simple() {
    let name = weave_dir_name("web-app", &WeaveName::new("agent-42"));
    assert_eq!(name, "web-app--agent-42");
}

#[test]
fn weave_dir_name_single_word_components() {
    let name = weave_dir_name("myproject", &WeaveName::new("hotfix"));
    assert_eq!(name, "myproject--hotfix");
}

#[test]
fn weave_dir_name_complex_primary() {
    let name = weave_dir_name("my-complex-project", &WeaveName::new("feat-login"));
    assert_eq!(name, "my-complex-project--feat-login");
}

#[test]
fn weave_dir_name_weave_with_numbers() {
    let name = weave_dir_name("app", &WeaveName::new("issue-1234"));
    assert_eq!(name, "app--issue-1234");
}

// ============================================================================
// parse_weave_dir_name — splits valid "{primary}--{weave}" names
// ============================================================================

#[test]
fn parse_valid_simple() {
    let result = parse_weave_dir_name("web-app--agent-42");
    let (primary, weave) = result.expect("should parse valid weave dir name");
    assert_eq!(primary, "web-app");
    assert_eq!(weave, WeaveName::new("agent-42"));
}

#[test]
fn parse_valid_single_word() {
    let (primary, weave) = parse_weave_dir_name("proj--fix").unwrap();
    assert_eq!(primary, "proj");
    assert_eq!(weave, WeaveName::new("fix"));
}

#[test]
fn parse_valid_hyphenated_components() {
    let (primary, weave) = parse_weave_dir_name("my-app--my-feature").unwrap();
    assert_eq!(primary, "my-app");
    assert_eq!(weave, WeaveName::new("my-feature"));
}

// ============================================================================
// parse_weave_dir_name — edge cases
// ============================================================================

#[test]
fn parse_no_double_dash_returns_none() {
    assert!(parse_weave_dir_name("web-app").is_none());
}

#[test]
fn parse_single_dash_returns_none() {
    assert!(parse_weave_dir_name("web-app-feature").is_none());
}

#[test]
fn parse_empty_string_returns_none() {
    assert!(parse_weave_dir_name("").is_none());
}

#[test]
fn parse_empty_primary_returns_none() {
    // "--weave" has empty primary
    assert!(parse_weave_dir_name("--weave").is_none());
}

#[test]
fn parse_empty_weave_returns_none() {
    // "primary--" has empty weave
    assert!(parse_weave_dir_name("primary--").is_none());
}

#[test]
fn parse_only_double_dash_returns_none() {
    assert!(parse_weave_dir_name("--").is_none());
}

#[test]
fn parse_multiple_double_dashes_splits_at_first() {
    // "a--b--c" should split at first "--" giving primary="a", weave="b--c"
    let (primary, weave) = parse_weave_dir_name("a--b--c").unwrap();
    assert_eq!(primary, "a");
    assert_eq!(weave, WeaveName::new("b--c"));
}

#[test]
fn parse_multiple_double_dashes_preserves_remainder() {
    let (primary, weave) = parse_weave_dir_name("proj--feat--v2--rc1").unwrap();
    assert_eq!(primary, "proj");
    assert_eq!(weave, WeaveName::new("feat--v2--rc1"));
}

// ============================================================================
// Round-trip: weave_dir_name -> parse_weave_dir_name
// ============================================================================

#[test]
fn round_trip_simple() {
    let primary = "web-app";
    let weave = WeaveName::new("agent-42");
    let dir_name = weave_dir_name(primary, &weave);
    let (parsed_primary, parsed_weave) = parse_weave_dir_name(&dir_name).unwrap();
    assert_eq!(parsed_primary, primary);
    assert_eq!(parsed_weave, weave);
}

#[test]
fn round_trip_single_char_components() {
    let primary = "a";
    let weave = WeaveName::new("b");
    let dir_name = weave_dir_name(primary, &weave);
    let (parsed_primary, parsed_weave) = parse_weave_dir_name(&dir_name).unwrap();
    assert_eq!(parsed_primary, primary);
    assert_eq!(parsed_weave, weave);
}

// ============================================================================
// read_active_project — reads .rwv-active file
// ============================================================================

#[test]
fn read_active_project_returns_none_when_no_file() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_active_project(tmp.path()).is_none());
}

#[test]
fn read_active_project_returns_name_from_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".rwv-active"), "my-project\n").unwrap();
    let project = read_active_project(tmp.path()).expect("should read project name");
    assert_eq!(project.as_str(), "my-project");
}

#[test]
fn read_active_project_returns_none_for_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".rwv-active"), "").unwrap();
    assert!(read_active_project(tmp.path()).is_none());
}

// ============================================================================
// set_active_project — writes .rwv-active file
// ============================================================================

#[test]
fn set_active_project_creates_file() {
    let tmp = tempfile::tempdir().unwrap();
    let project = ProjectName::new("web-app");
    set_active_project(tmp.path(), &project).unwrap();
    let content = std::fs::read_to_string(tmp.path().join(".rwv-active")).unwrap();
    assert_eq!(content, "web-app\n");
}

#[test]
fn set_active_project_round_trips_with_read() {
    let tmp = tempfile::tempdir().unwrap();
    let project = ProjectName::new("mobile-app");
    set_active_project(tmp.path(), &project).unwrap();
    let result = read_active_project(tmp.path()).expect("should read back");
    assert_eq!(result, project);
}
