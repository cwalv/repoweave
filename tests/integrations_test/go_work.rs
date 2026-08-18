// ===========================================================================
// go-work
// ===========================================================================

use super::*;

#[test]
fn auto_detects_repos_with_go_mod() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // go work use requires valid go.mod files (not just empty touches).
    write_file(
        root,
        "github/acme/server/go.mod",
        "module github.com/acme/server\n\ngo 1.21\n",
    );
    write_file(
        root,
        "github/acme/web/go.mod",
        "module github.com/acme/web\n\ngo 1.21\n",
    );
    touch(root, "github/acme/docs/README.md");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
        ("github/acme/docs", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = GoWork;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(content.contains("github/acme/server"));
    assert!(content.contains("github/acme/web"));
    assert!(!content.contains("github/acme/docs"));
}

#[test]
fn generates_go_work_with_use_directives() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "github/chatly/protocol/go.mod",
        "module github.com/chatly/protocol\n\ngo 1.21\n",
    );
    write_file(
        root,
        "github/chatly/server/go.mod",
        "module github.com/chatly/server\n\ngo 1.21\n",
    );

    let manifest = make_manifest(vec![
        ("github/chatly/protocol", Role::Owned),
        ("github/chatly/server", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = GoWork;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();
    // New behavior (merge port): the file includes the ownership marker
    // and uses tab-indented `use` blocks (go tool format).
    // Assert structural content rather than exact string (format varies).
    assert!(
        content.contains("./github/chatly/protocol"),
        "protocol path missing: {content}"
    );
    assert!(
        content.contains("./github/chatly/server"),
        "server path missing: {content}"
    );
    assert!(
        content.contains("// managed by repoweave"),
        "marker missing: {content}"
    );
}

#[test]
fn excludes_reference_repos() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // go work use requires valid go.mod files.
    write_file(
        root,
        "github/acme/server/go.mod",
        "module github.com/acme/server\n\ngo 1.21\n",
    );
    write_file(
        root,
        "github/acme/reference-lib/go.mod",
        "module github.com/acme/reference-lib\n\ngo 1.21\n",
    );

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/reference-lib", Role::Reference),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = GoWork;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(content.contains("github/acme/server"));
    assert!(!content.contains("reference-lib"));
}

#[test]
fn deactivation_removes_go_work_when_marker_present_and_only_rwv_content() {
    // New behavior (merge port): deactivate strips the managed `use` block
    // and deletes the file only when nothing user-authored remains.
    // A file with no marker is left untouched (user holds the pen).
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // File with marker + use block (no replace/toolchain/godebug = "empty").
    write_file(
        root,
        "go.work",
        "go 1.21\n\n// managed by repoweave\nuse (\n\t./github/acme/server\n)\n",
    );
    assert!(root.join("go.work").exists());

    let integration = GoWork;
    integration.deactivate(root).unwrap();
    // File deleted: only go/use content remained.
    assert!(!root.join("go.work").exists());
}

#[test]
fn deactivation_noop_when_no_marker() {
    // User-authored go.work without the rwv marker is left untouched.
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(root, "go.work", "go 1.21\n\nuse (\n)\n");
    assert!(root.join("go.work").exists());

    let integration = GoWork;
    integration.deactivate(root).unwrap();
    // File untouched: no marker present.
    assert!(root.join("go.work").exists());
}

#[cfg(unix)]
#[test]
fn check_warns_when_go_not_on_path() {
    let absent = doctor_json_on_tool_only_path(
        "go-work",
        "github/acme/server/go.mod",
        "module github.com/acme/server\n\ngo 1.20\n",
        &[],
    );
    let present = doctor_json_on_tool_only_path(
        "go-work",
        "github/acme/server/go.mod",
        "module github.com/acme/server\n\ngo 1.20\n",
        &["go"],
    );

    tool_missing_fires_then_clears(&absent, &present, "go-work", "go");
}

// -----------------------------------------------------------------------
// go-work — RED scenarios (turned green by C11)
// -----------------------------------------------------------------------
//
// A real weave's go.work carries `go 1.26` and a `use(...)` block over
// its members. The member names here are illustrative: `repoweave` and
// `some-go-tool` stand in for whatever a given weave actually holds.
//
// The hand-parse fallback is mandatory: the merge-logic tests
// must exercise the fallback deterministically. The current impl always
// overwrites and does not use `go work edit`, so for now we exercise the
// hand-parse fallback path implicitly (no `go work edit` exists).
//
// s6_go_1 and s6_go_2 pin their go.work/go.mod fixtures at 1.20, not this
// file's 1.26: both go through activate() with `go` on PATH, and 1.21 is
// the oldest go release with GOTOOLCHAIN switching, so a fixture at or
// below that never makes `go work` reach the network for a toolchain
// download. s6_go_3 and s6_go_4 go through deactivate(), which never
// invokes `go`, so they keep 1.26.

/// Adding a repo preserves a hand-authored `replace` directive.
/// `go 1.20` must NOT be downgraded to `1.21` (the concrete bug).
#[test]
fn s6_go_1_add_preserves_replace_and_go_version() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // go.mod files declare go 1.20 to match the go.work version.
    // When go is on PATH (primary path), max_go_version is computed from
    // these files; matching the go.work version prevents a downgrade.
    write_file(
        root,
        "github/cwalv/repoweave/go.mod",
        "module github.com/cwalv/repoweave\n\ngo 1.20\n",
    );
    write_file(
        root,
        "github/cwalv/some-go-tool/go.mod",
        "module github.com/cwalv/some-go-tool\n\ngo 1.20\n",
    );
    write_file(
        root,
        "github/cwalv/another-module/go.mod",
        "module github.com/cwalv/another-module\n\ngo 1.20\n",
    );

    // Pre-existing go.work with go 1.20, two members, a replace + comment.
    write_file(
        root,
        "go.work",
        r#"go 1.20

// managed by repoweave
use (
    ./github/cwalv/repoweave
    ./github/cwalv/some-go-tool
)

// pin local fork for the legacy migration
replace example.com/legacy => ./vendor/legacy
"#,
    );

    let manifest = make_manifest(vec![
        ("github/cwalv/repoweave", Role::Owned),
        ("github/cwalv/some-go-tool", Role::Owned),
        ("github/cwalv/another-module", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    GoWork.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(
        content.contains("github/cwalv/repoweave"),
        "use must include repoweave; got:\n{content}"
    );
    assert!(
        content.contains("github/cwalv/some-go-tool"),
        "use must include some-go-tool; got:\n{content}"
    );
    assert!(
        content.contains("github/cwalv/another-module"),
        "use must include the newly-added another-module; got:\n{content}"
    );
    assert!(
        content.contains("go 1.20"),
        "go 1.20 must survive (NOT downgraded to 1.21); got:\n{content}"
    );
    assert!(
        !content.contains("go 1.21"),
        "the 1.21 downgrade is the concrete bug; must not appear; got:\n{content}"
    );
    assert!(
        content.contains("replace example.com/legacy => ./vendor/legacy"),
        "replace directive must survive; got:\n{content}"
    );
    assert!(
        content.contains("// pin local fork for the legacy migration"),
        "user comment must survive; got:\n{content}"
    );
}

/// Removing a repo strips its use entry but keeps toolchain.
#[test]
fn s6_go_2_remove_keeps_toolchain_and_godebug() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    // go.mod files declare go 1.20 to match the go.work version (avoids
    // primary-path downgrade when go is on PATH and max_go_version is computed).
    write_file(
        root,
        "github/cwalv/repoweave/go.mod",
        "module github.com/cwalv/repoweave\n\ngo 1.20\n",
    );
    write_file(
        root,
        "github/cwalv/some-go-tool/go.mod",
        "module github.com/cwalv/some-go-tool\n\ngo 1.20\n",
    );
    // another-module is in the go.work seed but being removed from the manifest.
    // Its go.mod must exist on disk so the primary-path `go work use` for the
    // kept repos succeeds (go validates all existing use entries on modification).
    write_file(
        root,
        "github/cwalv/another-module/go.mod",
        "module github.com/cwalv/another-module\n\ngo 1.20\n",
    );

    write_file(
        root,
        "go.work",
        r#"go 1.20

toolchain go1.20.0

godebug default=go1.20

// managed by repoweave
use (
    ./github/cwalv/repoweave
    ./github/cwalv/some-go-tool
    ./github/cwalv/another-module
)
"#,
    );

    // another-module no longer in manifest.
    let manifest = make_manifest(vec![
        ("github/cwalv/repoweave", Role::Owned),
        ("github/cwalv/some-go-tool", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    GoWork.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(content.contains("./github/cwalv/repoweave"));
    assert!(content.contains("./github/cwalv/some-go-tool"));
    assert!(
        !content.contains("./github/cwalv/another-module"),
        "removed member must be gone from use; got:\n{content}"
    );
    assert!(
        content.contains("toolchain go1.20.0"),
        "toolchain must survive; got:\n{content}"
    );
    assert!(
        content.contains("godebug default=go1.20"),
        "godebug must survive; got:\n{content}"
    );
    assert!(
        content.contains("go 1.20"),
        "go version must survive; got:\n{content}"
    );
}

/// Deactivate strips the use set but keeps replace.
/// Regression vs current unconditional remove_file at go_work.rs:36-38.
#[test]
fn s6_go_3_deactivate_strips_use_keeps_replace() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "go.work",
        r#"go 1.26

// managed by repoweave
use (
    ./github/cwalv/repoweave
)

replace example.com/foo => ../foo
"#,
    );

    GoWork.deactivate(root).unwrap();

    assert!(
        root.join("go.work").exists(),
        "go.work must NOT be deleted when foreign content (replace) remains"
    );
    let content = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(
        !content.contains("./github/cwalv/repoweave"),
        "use entries must be stripped; got:\n{content}"
    );
    assert!(
        !content.contains("// managed by repoweave"),
        "marker must be stripped; got:\n{content}"
    );
    assert!(
        content.contains("go 1.26"),
        "go version must survive; got:\n{content}"
    );
    assert!(
        content.contains("replace example.com/foo => ../foo"),
        "replace must survive; got:\n{content}"
    );
}

/// Deactivate deletes when only rwv content remained.
///
/// Currently GREEN incidentally — current go.work deactivate is an
/// unconditional `remove_file`, which happens to satisfy this scenario.
/// Keep ungated as a regression guard against the C11 port: when C11
/// switches to strip-not-delete-with-delete-if-empty, this scenario must
/// still hold (file deleted because the post-strip doc is empty).
#[test]
fn s6_go_4_deactivate_deletes_when_only_rwv_content() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "go.work",
        r#"go 1.26

// managed by repoweave
use (
    ./github/cwalv/repoweave
    ./github/cwalv/some-go-tool
)
"#,
    );

    GoWork.deactivate(root).unwrap();

    assert!(
        !root.join("go.work").exists(),
        "go.work must be deleted: only rwv-authored content (go line + use) remains"
    );
}

/// This module asserts on `go.work` by searching the whole file for a
/// member path, and the whole file is not the owned region: `replace`
/// directives and every comment are user content rwv carries through. So a
/// path sitting in either is enough to satisfy such a search without being
/// a `use` member at all.
///
/// Latent exposure, not a live defect — no sibling fixture above puts a
/// member-shaped path anywhere but the `use` block, so every one of them is
/// correct today. This is the fixture that tells the two apart, and it
/// asserts through `verify()`, which compares the on-disk `use` set against
/// the manifest rather than searching text.
///
/// Pinned at `go 1.20` for the same reason the scenarios above are: with
/// `go` on PATH this runs the `go work` path, and 1.21 is the oldest
/// release that would send it to the network for a toolchain.
#[test]
fn a_decoy_path_in_a_comment_or_replace_is_not_a_use_member() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "github/acme/server/go.mod",
        "module github.com/acme/server\n\ngo 1.20\n",
    );

    write_file(
        root,
        "go.work",
        r#"go 1.20

// ./github/acme/decoy left the weave; the note is kept on purpose
replace example.com/decoy => ./github/acme/decoy
"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    GoWork.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(
        content.contains("github/acme/decoy"),
        "fixture is inert unless the decoy survives activate; got:\n{content}"
    );

    let issues = GoWork.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "the decoy is user content, so the authored use set is exactly the \
             manifest's members and verify has nothing to report; got: {issues:?}"
    );
}
